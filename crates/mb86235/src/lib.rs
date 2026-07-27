//! Fujitsu MB86235 "TGPx4" DSP, the geometry coprocessor on the Sega Model 2C
//! video board (Wave Runner, The House of the Dead, Top Skater, Sega Ski,...).
//!
//! The part
//! is a 64-bit-instruction VLIW DSP: each word carries an ALU operation and one
//! or two transfer operations that issue together. Instructions are dispatched
//! on `op[63:61]`, which selects how those slots are packed.
//!
//! On Model 2C the i960 uploads the microcode a 32-bit half at a time into the
//! 4096-entry program RAM, then releases the core; from there it reads command
//! words from the input FIFO, transforms geometry, and writes results to the
//! output FIFO -- the same contract the MB86234 TGP and the SHARC fulfil on the
//! earlier boards.
//!
//! `jit` is a Cranelift block dynarec over the same state, gated at runtime
//! by `Mb86235::jit_enabled`; with it off (or the feature disabled) the core
//! is exactly the interpreter.

mod alu;
#[cfg(feature = "jit")]
pub mod jit;
mod memory;
pub mod ops;
mod state;
mod trans;

pub use state::Mb86235;

/// The world outside the DSP's own program RAM: the Model 2 coprocessor FIFOs,
/// the buffer RAM it shares with the i960, and the coprocessor data ROM.
pub trait Mb86235Bus {
    /// Read a 32-bit word from the DSP's external data space.
    fn data_read(&mut self, addr: u32) -> u32;
    /// Write a 32-bit word to the DSP's external data space.
    fn data_write(&mut self, addr: u32, data: u32);
    /// Pop a command word from the input FIFO, if one is waiting.
    fn fifo_in_pop(&mut self) -> Option<u32>;
    /// Push a result word to the output FIFO.
    fn fifo_out_push(&mut self, data: u32);
    /// FIFO state, as the IFF/IFE/OFF/OFE branch conditions read it.
    fn fifo_in_empty(&self) -> bool {
        true
    }
    fn fifo_in_full(&self) -> bool {
        false
    }
    fn fifo_out_empty(&self) -> bool {
        true
    }
    fn fifo_out_full(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullBus;
    impl Mb86235Bus for NullBus {
        fn data_read(&mut self, _a: u32) -> u32 {
            0
        }
        fn data_write(&mut self, _a: u32, _d: u32) {}
        fn fifo_in_pop(&mut self) -> Option<u32> {
            None
        }
        fn fifo_out_push(&mut self, _d: u32) {}
    }

    /// An input FIFO preloaded with command words; the output FIFO drains
    /// into a vector.
    struct FiFoBus {
        input: std::collections::VecDeque<u32>,
        output: Vec<u32>,
    }
    impl Mb86235Bus for FiFoBus {
        fn data_read(&mut self, _a: u32) -> u32 {
            0
        }
        fn data_write(&mut self, _a: u32, _d: u32) {}
        fn fifo_in_pop(&mut self) -> Option<u32> {
            self.input.pop_front()
        }
        fn fifo_out_push(&mut self, d: u32) {
            self.output.push(d);
        }
        fn fifo_in_empty(&self) -> bool {
            self.input.is_empty()
        }
    }

    /// Class 1: a dual ALU slot (an ALU op plus an integer multiply) with one
    /// internal transfer. `low12` carries the mode/ry/disp fields (and the
    /// literal when the source is 0x58).
    #[allow(clippy::too_many_arguments)]
    fn alu2_t1(
        aop: u64,
        ai1: u64,
        ai2: u64,
        ao: u64,
        mi1: u64,
        mi2: u64,
        mo: u64,
        sr: u64,
        dr: u64,
        low12: u64,
    ) -> u64 {
        (1 << 61)
            | (aop << 56)
            | (ai1 << 52)
            | (ai2 << 47)
            | (ao << 42)
            | (mi1 << 37)
            | (mi2 << 32)
            | (mo << 27)
            | (sr << 19)
            | (dr << 12)
            | (low12 & 0xfff)
    }

    /// Control class (bit 63 set, so a one-operand ALU NOP slot issues
    /// alongside; ai2 = 0 keeps it off the PR ring).
    fn ctl(cop: u64, ef1: u64, ef2: u64) -> u64 {
        (6 << 61) | (0x07 << 56) | (1 << 41) | (cop << 22) | (ef1 << 16) | (ef2 & 0xffff)
    }

    /// The differential program: ALU and multiplier slots, register and
    /// memory transfers with a post-incrementing EA, a REP, a call/return
    /// with delay slots, a DJMP loop, and an input-FIFO stall once the one
    /// supplied command word has been consumed.
    fn build(c: &mut Mb86235) {
        c.aa[0] = 3;
        c.aa[1] = 4;
        c.ma[0] = 2;
        c.ar[0] = 5;
        c.dataa[5] = 0x1234_5678;
        c.program[0x000] = alu2_t1(0x10, 0, 1, 0x10, 0, 0, 0, 0x00, 0x01, 0); // aa0 += aa1; ma1 = ma0; ma0 *= ma0
        c.program[0x001] = alu2_t1(0x07, 0, 0, 0x10, 0, 0, 0, 0x40, 0x02, 0x001); // ma2 = dataa[ar0++]
        c.program[0x002] = ctl(0x01, 0, 2); // REP 2
        c.program[0x003] = alu2_t1(0x10, 0, 1, 0x13, 0, 0, 0, 0x00, 0x01, 0); // aa3 = aa0 + aa1
        c.program[0x004] = ctl(0x1a, 0, 0x010); // DCALL 0x010
        c.program[0x005] = alu2_t1(0x00, 0, 1, 0x14, 0, 0, 0, 0x00, 0x01, 0); // delay: FADD aa4
        c.program[0x006] = alu2_t1(0x07, 0, 0, 0x10, 0, 0, 0, 0x31, 0x03, 0); // ma3 = FI pop
        c.program[0x007] = ctl(0x12, 0, 0x006); // DJMP 0x006
        c.program[0x008] = alu2_t1(0x07, 0, 0, 0x10, 0, 0, 0, 0x03, 0x04, 0); // delay: ma4 = ma3
        c.program[0x010] = alu2_t1(0x12, 0, 1, 0x15, 0, 0, 0, 0x00, 0x01, 0); // aa5 = aa1 - aa0
        c.program[0x011] = ctl(0x1b, 0, 0); // DRET
        c.program[0x012] = alu2_t1(0x07, 0, 0, 0x10, 0, 0, 0, 0x01, 0x01, 0); // delay: ma1 = ma1
    }

    /// Serializes the tests that flip the global `jit::BLOCK_CAP`.
    static JIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Diffs the full architected state of two cores that ran the same
    /// program (the interpreter reference and the JIT under test).
    fn assert_same_state(i: &Mb86235, j: &Mb86235) {
        assert_eq!(i.pc, j.pc, "pc");
        assert_eq!(i.delay_pc, j.delay_pc, "delay_pc");
        assert_eq!(i.ppc, j.ppc, "ppc");
        assert_eq!(i.delay_slot, j.delay_slot, "delay_slot");
        assert_eq!(i.pcs, j.pcs, "pcs");
        assert_eq!(i.pcp, j.pcp, "pcp");
        assert_eq!(i.aa, j.aa, "aa");
        assert_eq!(i.ab, j.ab, "ab");
        assert_eq!(i.ma, j.ma, "ma");
        assert_eq!(i.mb, j.mb, "mb");
        assert_eq!(i.ar, j.ar, "ar");
        assert_eq!(i.sp, j.sp, "sp");
        assert_eq!(i.eb, j.eb, "eb");
        assert_eq!(i.eo, j.eo, "eo");
        assert_eq!(i.rpc, j.rpc, "rpc");
        assert_eq!(i.lpc, j.lpc, "lpc");
        assert_eq!(i.mod_, j.mod_, "mod_");
        assert_eq!(i.st, j.st, "st");
        assert_eq!(i.pr, j.pr, "pr");
        assert_eq!(i.prp, j.prp, "prp");
        assert_eq!(i.pwp, j.pwp, "pwp");
        assert_eq!(i.pdr, j.pdr, "pdr");
        assert_eq!(i.ddr, j.ddr, "ddr");
        assert_eq!(i.dataa, j.dataa, "dataa");
        assert_eq!(i.datab, j.datab, "datab");
        assert_eq!(i.stalled, j.stalled, "stalled");
        assert_eq!(i.stall_pc, j.stall_pc, "stall_pc");
        assert_eq!(i.icount, j.icount, "icount");
        assert_eq!(i.insns, j.insns, "insns");
        assert_eq!(i.unimpl, j.unimpl, "unimpl");
    }

    /// Runs `program` on both engines (the JIT with single-instruction
    /// blocks, so both retire exactly `cycles` instructions) and diffs the
    /// architected state.
    #[cfg(feature = "jit")]
    fn dual_run(program: impl Fn(&mut Mb86235), cycles: i32) {
        let _guard = JIT_TEST_LOCK.lock().unwrap();
        let mut bus_i = NullBus;
        let mut interp = Mb86235::new();
        program(&mut interp);
        interp.jit_enabled = false;
        interp.execute(&mut bus_i, cycles);

        let mut bus_j = NullBus;
        let mut jitted = Mb86235::new();
        program(&mut jitted);
        crate::jit::BLOCK_CAP.store(1, std::sync::atomic::Ordering::Relaxed);
        jitted.execute(&mut bus_j, cycles);
        crate::jit::BLOCK_CAP.store(0, std::sync::atomic::Ordering::Relaxed);

        // The test only means something if the JIT engine actually ran.
        let jit = jitted
            .jit
            .0
            .as_ref()
            .and_then(|j| j.downcast_ref::<crate::jit::Jit>())
            .expect("jit cache present");
        assert!(jit.compiles > 0, "the JIT path compiled blocks");

        assert_same_state(&interp, &jitted);
    }

    /// Class 0, sd == 0: a dual ALU slot plus two register transfers.
    /// `wa` is a 5-bit A-side register, `wb4` the 4-bit mb/ab selector.
    #[allow(clippy::too_many_arguments)]
    fn alu2_t2(
        aop: u64,
        ai1: u64,
        ai2: u64,
        ao: u64,
        mop: u64,
        mi1: u64,
        mi2: u64,
        mo: u64,
        wa: u64,
        wb4: u64,
    ) -> u64 {
        (aop << 56)
            | (ai1 << 52)
            | (ai2 << 47)
            | (ao << 42)
            | (mop << 41)
            | (mi1 << 37)
            | (mi2 << 32)
            | (mo << 27)
            | ((wa & 0x1f) << 20)
            | ((wb4 & 0xf) << 10)
    }

    /// Class 4, sda == sdb == 0: a single ALU slot plus two register
    /// transfers (`wb5`/`wb_d5` are the 5-bit fields that gain 0x20).
    #[allow(clippy::too_many_arguments)]
    fn alu1_t2(aop: u64, ai1: u64, ai2: u64, ao: u64, wa: u64, wa_d: u64, wb5: u64, wb_d5: u64) -> u64 {
        (4 << 61)
            | (1 << 41)
            | (aop << 56)
            | (ai1 << 52)
            | (ai2 << 47)
            | (ao << 42)
            | ((wa & 0x1f) << 33)
            | ((wa_d & 0x1f) << 28)
            | ((wb5 & 0x1f) << 13)
            | ((wb_d5 & 0x1f) << 8)
    }

    /// Class 5: a single ALU slot plus one register transfer. With `mul` the
    /// slot is the multiplier instead (`aop` != 0 selects FMUL, and the ai/ao
    /// fields address the multiplier registers).
    #[allow(clippy::too_many_arguments)]
    fn alu1_t1(mul: bool, aop: u64, ai1: u64, ai2: u64, ao: u64, sr: u64, dr: u64) -> u64 {
        (5 << 61)
            | ((!mul as u64) << 41)
            | (aop << 56)
            | (ai1 << 52)
            | (ai2 << 47)
            | (ao << 42)
            | ((sr & 0x7f) << 31)
            | ((dr & 0x7f) << 24)
    }

    /// Class 7: a 32-bit immediate to a register.
    fn imm_t1(dr: u64, imm: u64) -> u64 {
        (7 << 61) | ((imm & 0xffff_ffff) << 27) | ((dr & 0x7f) << 19)
    }

    /// Sweeps every natively-lowered ALU/multiplier form and transfer class
    /// (0/4/5/7): float and integer arithmetic with the Z-clamp variants,
    /// compares, logicals, shifts, CIF/CFI, FMUL/IMUL, the constant tables,
    /// the EB split reads/writes, a read of ST, and group-7 (read 0 / drop
    /// write) register codes.
    #[test]
    #[cfg(feature = "jit")]
    fn jit_matches_interpreter_alu_sweep() {
        dual_run(
            |c| {
                c.aa[0] = 3;
                c.aa[1] = 4;
                c.aa[2] = 0x8000_0001;
                c.aa[3] = f32::to_bits(1.5);
                c.aa[4] = f32::to_bits(-2.0);
                c.aa[5] = 0xffff_fff0;
                c.ma[0] = f32::to_bits(2.0);
                c.ma[1] = 7;
                c.ma[2] = f32::to_bits(-0.5);
                c.mb[0] = f32::to_bits(0.25);
                let p = &mut c.program;
                // Class 7 immediates: ar (14-bit mask), the EB split write.
                p[0x00] = imm_t1(0x18, 0x0000_9abc); // ar0
                p[0x01] = imm_t1(0x11, 0x0012_3456); // eb[23:14]
                // Class 0: FADD aa2 = aa0 + aa1 (int bits as floats), FMUL
                // mb1 = ma0 * 1.0 (const table), ma0->ma0, mb0->ab0... the
                // transfers are self-copies (sd == 0 writes the same field).
                p[0x02] = alu2_t2(0x00, 0, 1, 0x12, 1, 0, 0x1b, 0x09, 0x00, 0x08);
                // Class 1: FSUBZ with a const second source (group 3).
                p[0x03] = alu2_t1(0x03, 1, 0x1b, 0x13, 0, 0, 0, 0x00, 0x01, 0);
                // Class 1: FCMP (flags only) + IMUL mb3 = ma1 * -1 (const).
                p[0x04] = alu2_t1(0x04, 3, 4, 0x10, 1, 0x1a, 0x0b, 0x00, 0x01, 0);
                // Class 4: SHL aa3 = aa2 << 3, transfers aa0->ar1, mb0->ab0.
                p[0x05] = alu1_t2(0x1d, 2, 3, 0x13, 0x00, 0x19, 0x00, 0x08);
                // Class 4: SAR ab1 = aa2 >> 5, transfers eb->ma4, ab0->mb4.
                p[0x06] = alu1_t2(0x1e, 2, 5, 0x11, 0x10, 0x04, 0x08, 0x04);
                // Class 5: CIF ab2 = (float)aa1; st -> ab3 (control read).
                p[0x07] = alu1_t1(false, 0x0d, 1, 0, 0x1a, 0x15, 0x2b);
                // Class 5: CFI ma5 = (int)aa3 (1.5 -> 1); group 7 src reads 0.
                p[0x08] = alu1_t1(false, 0x0e, 3, 0, 0x05, 0x38, 0x05);
                // Class 5 multiplier form: FMUL mb5 = ma0 * -1.0 (const).
                p[0x09] = alu1_t1(true, 1, 0, 0x18, 0x0d, 0x00, 0x2d);
                // Class 5 multiplier form: IMUL ma6 = ma1 * 0 (const).
                p[0x0a] = alu1_t1(true, 0, 1, 0x18, 0x06, 0x00, 0x06);
                // Class 1: FABS + FMUL; ATRZ; logical AND/OR/XOR/NOT.
                p[0x0b] = alu2_t1(0x05, 4, 0, 0x14, 0, 0, 0, 0x00, 0x01, 0);
                p[0x0c] = alu2_t1(0x17, 2, 0, 0x15, 0, 0, 0, 0x00, 0x01, 0);
                p[0x0d] = alu2_t1(0x18, 0, 5, 0x16, 0, 0, 0, 0x00, 0x01, 0);
                p[0x0e] = alu2_t1(0x19, 0, 1, 0x17, 0, 0, 0, 0x00, 0x01, 0);
                p[0x0f] = alu2_t1(0x1a, 0, 5, 0x10, 0, 0, 0, 0x00, 0x01, 0);
                p[0x10] = alu2_t1(0x1b, 0, 0, 0x11, 0, 0, 0, 0x00, 0x01, 0);
                // Integer ADD/ADDZ/SUB/SUBZ/CMP/ABS/ATR, SHR/SAL.
                p[0x11] = alu2_t1(0x10, 0, 1, 0x10, 0, 0, 0, 0x00, 0x01, 0);
                p[0x12] = alu2_t1(0x11, 0, 2, 0x11, 0, 0, 0, 0x00, 0x01, 0);
                p[0x13] = alu2_t1(0x12, 0, 1, 0x12, 0, 0, 0, 0x00, 0x01, 0);
                p[0x14] = alu2_t1(0x13, 1, 0, 0x13, 0, 0, 0, 0x00, 0x01, 0);
                p[0x15] = alu2_t1(0x14, 0, 1, 0x10, 0, 0, 0, 0x00, 0x01, 0);
                p[0x16] = alu2_t1(0x15, 2, 0, 0x14, 0, 0, 0, 0x00, 0x01, 0);
                p[0x17] = alu2_t1(0x16, 5, 0, 0x15, 0, 0, 0, 0x00, 0x01, 0);
                p[0x18] = alu2_t1(0x1c, 5, 4, 0x16, 0, 0, 0, 0x00, 0x01, 0);
                p[0x19] = alu2_t1(0x1f, 1, 2, 0x17, 0, 0, 0, 0x00, 0x01, 0);
                // Class 7 immediate to a group-7 register: dropped.
                p[0x1a] = imm_t1(0x39, 0xffff_ffff);
            },
            40,
        );
    }

    /// Runs the same program through the interpreter and the JIT (with
    /// single-instruction blocks, so both retire exactly `cycles`
    /// instructions) and diffs the full architected state. Covers REP, a
    /// call/return with delay slots, a DJMP loop, and a FIFO stall.
    #[test]
    #[cfg(feature = "jit")]
    fn jit_matches_interpreter() {
        let _guard = JIT_TEST_LOCK.lock().unwrap();
        let mut bus_i = FiFoBus {
            input: [0xdead_beef].into(),
            output: Vec::new(),
        };
        let mut interp = Mb86235::new();
        build(&mut interp);
        interp.jit_enabled = false;
        interp.execute(&mut bus_i, 40);

        let mut bus_j = FiFoBus {
            input: [0xdead_beef].into(),
            output: Vec::new(),
        };
        let mut jitted = Mb86235::new();
        build(&mut jitted);
        crate::jit::BLOCK_CAP.store(1, std::sync::atomic::Ordering::Relaxed);
        jitted.execute(&mut bus_j, 40);
        crate::jit::BLOCK_CAP.store(0, std::sync::atomic::Ordering::Relaxed);

        // Sanity: the program did what the comments claim, on both engines.
        assert_eq!(interp.aa[0], 7, "aa0 = 3 + 4");
        assert_eq!(interp.ma[2], 0x1234_5678, "EA read");
        assert_eq!(interp.ar[0], 6, "EA post-increment");
        assert_eq!(interp.ma[4], 0xdead_beef, "FIFO pop, copied before the stall");
        // The stalled retry re-issues the transfer with no FIFO word: the
        // interpreter's `write_dst` retires the placeholder 0. (Both engines
        // go through the same `execute_op`, so they agree either way.)
        assert_eq!(interp.ma[3], 0, "stalled retry clobbers the destination");
        assert!(interp.stalled, "second pop stalls on the empty FIFO");

        assert_same_state(&interp, &jitted);
        assert_eq!(bus_i.output, bus_j.output, "output FIFO");
    }

    /// An upload to a page a cached block spans must recompile it: after
    /// replacing the first program word, re-running from 0 picks up the new
    /// immediate, not the block compiled for the old one.
    #[test]
    #[cfg(feature = "jit")]
    fn jit_upload_invalidates_blocks() {
        let nop = ctl(0, 0, 0);
        // Class 7: transfer a 32-bit immediate to ma0 (dr = 0).
        let load_ma0 = |imm: u64| (7u64 << 61) | (imm << 27);
        let mut bus = NullBus;
        let mut c = Mb86235::new();
        c.program[0] = load_ma0(0x1111_1111);
        for w in c.program[1..8].iter_mut() {
            *w = nop;
        }
        c.execute(&mut bus, 4);
        assert_eq!(c.ma[0], 0x1111_1111);

        let w = load_ma0(0x2222_2222);
        c.upload_program_half(0, w as u32);
        c.upload_program_half(1, (w >> 32) as u32);
        c.pc = 0;
        c.execute(&mut bus, 4);
        assert_eq!(c.ma[0], 0x2222_2222, "recompiled after upload");
    }

    /// The interpreter path on its own (the JIT off switch is exact).
    #[test]
    fn interpreter_runs_program() {
        let mut bus = FiFoBus {
            input: [0xdead_beef].into(),
            output: Vec::new(),
        };
        let mut c = Mb86235::new();
        build(&mut c);
        c.jit_enabled = false;
        c.execute(&mut bus, 40);
        assert_eq!(c.aa[0], 7);
        assert_eq!(c.aa[3], 11);
        assert_eq!(c.ma[4], 0xdead_beef);
        assert!(c.stalled);
        assert_eq!(c.stall_pc, 0x006);
        assert_eq!(c.icount, 0);
        // 39 instructions retire: the DJMP charges one extra cycle.
        assert_eq!(c.insns, 39);
    }
}
