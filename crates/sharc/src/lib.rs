//! ADSP-21062 "SHARC" DSP core, as used as the geometry coprocessor on the
//! Sega Model 2B video board (Virtua Striker, Fighting Vipers, Virtua-On,...).
//!
//! The core executes the 48-bit SHARC microcode the game's
//! i960 uploads by DMA, reading commands and vertices from the input FIFO and
//! writing transformed geometry to the output FIFO, exactly where the Model 2
//! MB86234 TGP does on the 2A/original boards.
//!
//! Layout mirrors the reference so the instruction handlers (in `ops.rs`) port across
//! mechanically:
//!   * 40-bit register file `r[16]` (+ alternate bank), used as int or float
//!   * two data-address generators (DAG1 on the DM bus, DAG2 on the PM bus)
//!   * PC/loop/status stacks
//!   * program memory (48-bit) and data memory (32-bit) with internal SRAM
//!
//! The instruction set itself is filled in incrementally; see `ops.rs`.
//! `jit` is a Cranelift block dynarec over the same state, gated at runtime
//! by `Sharc::jit_enabled`; with it off (or the feature disabled) the core is
//! exactly the interpreter.

mod compute;
mod consts;
#[cfg(feature = "jit")]
pub mod jit;
mod memory;
pub mod ops;
mod regs;
mod state;
mod tables;

pub use state::{Sharc, SharcReg};

/// External bus the coprocessor reaches outside its internal SRAM: the Model 2
/// FIFOs, the shared buffer RAM, and the copro data ROM. The integration in
/// `motherboard` implements this over the same FIFOs the TGP uses.
pub trait SharcBus {
    /// Read a 32-bit word from the SHARC's external data space.
    fn dm_ext_read(&mut self, addr: u32) -> u32;
    /// Write a 32-bit word to the SHARC's external data space.
    fn dm_ext_write(&mut self, addr: u32, data: u32);
    /// Read a 48-bit word from the SHARC's external program space.
    fn pm_ext_read(&mut self, addr: u32) -> u64 {
        let _ = addr;
        0
    }
    /// Write a 48-bit word to the SHARC's external program space.
    fn pm_ext_write(&mut self, addr: u32, data: u64) {
        let _ = (addr, data);
    }
    /// FLAG0 input: the command/input FIFO is empty (microcode polls this
    /// before reading a command word).
    fn fifo_in_empty(&self) -> bool {
        true
    }
    /// FLAG1 input: the output FIFO is full (microcode polls this before
    /// writing a result word).
    fn fifo_out_full(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullBus;
    impl SharcBus for NullBus {
        fn dm_ext_read(&mut self, _a: u32) -> u32 {
            0
        }
        fn dm_ext_write(&mut self, _a: u32, _d: u32) {}
    }

    // A single-function ALU compute: Rn = Rx + Ry (op 0x01, cu 0).
    // Encoding: top 9 bits (opcode[47:39]) must select `Compute` (mask 0xff00,
    // value 0x0100 => op field 0x02 in bits[47:40]); cond in [37:33]=0x1f (TRUE),
    // compute field in [22:0]: op=0x01<<12, rn=2<<8, rx=0<<4, ry=1.
    #[test]
    fn alu_add_executes() {
        let mut c = Sharc::new();
        c.reset();
        c.r[0] = 5;
        c.r[1] = 7;
        let compute: u64 = ((0x01 << 12) | (2 << 8)) | 1;
        let cond: u64 = 0x1f;
        let opcode: u64 = (0x02u64 << 39) | (cond << 33) | compute;
        // Reset leaves daddr = pc + 1, so the first instruction actually
        // retired is the one at 0x20005.
        let base = 0x20005usize - 0x20000;
        c.pm[base] = opcode;
        let mut bus = NullBus;
        c.execute(&mut bus, 4);
        assert_eq!(c.r[2], 12, "R2 should be R0+R1");
    }

    // ALU compute encoding helper (op 0x02 class, cond, compute field).
    fn alu(op: u64, rn: u64, rx: u64, ry: u64) -> u64 {
        let compute = (op << 12) | (rn << 8) | (rx << 4) | ry;
        (0x02u64 << 39) | (0x1fu64 << 33) | compute
    }

    /// Runs the same program through the interpreter and the JIT (with
    /// single-instruction blocks, so both retire exactly `cycles`
    /// instructions) and diffs the architected state. Covers a hardware
    /// `DO UNTIL` loop closing mid-stream -- the JIT's loop guard and
    /// redirect path.
    #[test]
    #[cfg(feature = "jit")]
    fn jit_matches_interpreter() {
        let build = |c: &mut Sharc| {
            c.reset();
            c.r[0] = 5;
            c.r[1] = 7;
            let at = |c: &mut Sharc, addr: usize, w: u64| c.pm[addr - 0x20000] = w;
            // 0x20005: LCNTR=5, DO 0x20007 UNTIL LCE (offset = bottom - pc).
            let do_until: u64 = (0x18u64 << 39) | (5 << 24) | 2;
            at(c, 0x20005, do_until);
            at(c, 0x20006, alu(0x01, 2, 0, 1)); // r2 = r0 + r1
            at(c, 0x20007, alu(0x01, 0, 0, 1)); // r0 = r0 + r1 (loop bottom)
            at(c, 0x20008, alu(0x01, 3, 2, 0)); // r3 = r2 + r0
            at(c, 0x20009, alu(0x01, 4, 3, 1)); // r4 = r3 + r1
        };

        let mut bus = NullBus;
        let mut interp = Sharc::new();
        build(&mut interp);
        interp.jit_enabled = false;
        interp.execute(&mut bus, 40);

        let mut jitted = Sharc::new();
        build(&mut jitted);
        crate::jit::BLOCK_CAP.store(1, std::sync::atomic::Ordering::Relaxed);
        jitted.execute(&mut bus, 40);
        crate::jit::BLOCK_CAP.store(0, std::sync::atomic::Ordering::Relaxed);

        assert_eq!(interp.r, jitted.r, "register file");
        assert_eq!(interp.pc, jitted.pc, "pc");
        assert_eq!(interp.daddr, jitted.daddr, "daddr");
        assert_eq!(interp.faddr, jitted.faddr, "faddr");
        assert_eq!(interp.nfaddr, jitted.nfaddr, "nfaddr");
        assert_eq!(interp.astat, jitted.astat, "astat");
        assert_eq!(interp.icount, jitted.icount, "icount");
        assert_eq!(interp.insns, jitted.insns, "insns");
        assert_eq!(interp.lstkp, jitted.lstkp, "lstkp");
        assert_eq!(interp.curlcntr, jitted.curlcntr, "curlcntr");
    }
}
