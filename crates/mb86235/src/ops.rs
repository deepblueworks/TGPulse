//! Instruction decode and execution.
//!
//! The MB86235 is a VLIW: one 64-bit word issues an ALU operation together
//! with one or two transfer operations. `op[63:61]` selects the packing, which
//! is the whole of the reference dispatch:
//!
//!   0 ALU (2 operands) + two transfers
//!   1 ALU (2 operands) + one transfer
//!   2 control (branch / loop / call)
//!   4 ALU (1 operand)  + two transfers
//!   5 ALU (1 operand)  + one transfer
//!   6 control
//!   7 transfer only
//!
//! Handlers are ported from `mb86235ops.cpp` incrementally; classes without one
//! are counted in `unimpl` rather than silently doing nothing.

use crate::state::Mb86235;
use crate::Mb86235Bus;

/// The seven instruction classes, indexed by `op[63:61]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Alu2Trans2,
    Alu2Trans1,
    Control,
    Alu1Trans2,
    Alu1Trans1,
    Trans1,
    Illegal,
}

#[inline]
pub fn classify(op: u64) -> Class {
    match (op >> 61) & 7 {
        0 => Class::Alu2Trans2,
        1 => Class::Alu2Trans1,
        2 | 6 => Class::Control,
        4 => Class::Alu1Trans2,
        5 => Class::Alu1Trans1,
        7 => Class::Trans1,
        _ => Class::Illegal,
    }
}

impl Mb86235 {
    /// Runs the core for `cycles` instructions.
    pub fn execute<B: Mb86235Bus>(&mut self, bus: &mut B, cycles: i32) {
        self.icount = cycles;
        while self.icount > 0 {
            // A stalled instruction re-fetches from where it stalled, unless a
            // REP is active -- the repeat holds the PC itself.
            let curpc = if self.stalled && self.st & crate::state::flag::RP == 0 {
                self.stall_pc
            } else {
                self.pc
            };
            let op = self.fetch(curpc);
            self.ppc = curpc;

            if self.delay_slot {
                // A branch's delay slot runs the instruction after it, then
                // the branch target takes effect.
                self.pc = self.delay_pc;
                self.delay_slot = false;
            } else if !self.stalled {
                // REP re-executes the *same* instruction: the PC does not
                // advance while the repeat flag is live, it just counts down.
                // Missing this leaves the repeat permanently armed, which is
                // what stalled Wave Runner's coprocessor handshake.
                if self.st & crate::state::flag::RP != 0 {
                    self.rpc = self.rpc.wrapping_sub(1);
                    if self.rpc == 1 {
                        self.st &= !crate::state::flag::RP;
                    }
                } else {
                    self.pc = self.pc.wrapping_add(1);
                }
            }

            self.execute_op(bus, op);

            self.insns = self.insns.wrapping_add(1);
            self.icount -= 1;
        }
    }

    fn execute_op<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        // A stall is per-instruction: cleared before issue, set if a slot
        // pops an empty input FIFO, and re-run next cycle if so.
        self.stalled = false;
        match classify(op) {
            Class::Alu2Trans2 => self.do_alu2_trans2(bus, op),
            Class::Alu2Trans1 => self.do_alu2_trans1(bus, op),
            Class::Alu1Trans2 => self.do_alu1_trans2(bus, op),
            Class::Alu1Trans1 => self.do_alu1_trans1(bus, op),
            Class::Trans1 => self.do_trans1_imm(bus, op),
            Class::Control => self.do_control(bus, op),
            Class::Illegal => self.unimpl[3] += 1,
        }
        // A stalled instruction has not retired: remember where to resume.
        if self.stalled {
            self.stall_pc = self.ppc;
        }
    }

    // --- PC stack ---
    fn push_pc(&mut self, pcval: u32) {
        self.pcs[self.pcp as usize] = pcval;
        self.pcp = (self.pcp + 1) & 3;
    }

    fn pop_pc(&mut self) -> u32 {
        self.pcp = self.pcp.wrapping_sub(1) & 3;
        self.pcs[self.pcp as usize]
    }

    /// Branch/jump condition. Codes below 19
    /// test a status flag; the rest test the FIFO states.
    fn branch_cond<B: Mb86235Bus>(&self, bus: &B, which: u32) -> bool {
        use crate::state::flag::*;
        const TABLE: [u32; 19] = [
            MN, MZ, MV, MU, ZD, NR, IL, ZC, AN, AZ, AV, AU, MD, AD, 0, 0, F0, F1, F2,
        ];
        if (which as usize) < TABLE.len() {
            return self.st & TABLE[which as usize] != 0;
        }
        match which {
            20 => bus.fifo_in_full(),
            21 => bus.fifo_in_empty(),
            22 => bus.fifo_out_full(),
            23 => bus.fifo_out_empty(),
            _ => false,
        }
    }

    /// Branch destination: the mode field picks an
    /// immediate, a register, or a memory word, and bit 12 makes it relative.
    fn control_dst<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) -> u32 {
        let rx = ((op >> 6) & 7) as usize;
        let addr = match (op >> 13) & 7 {
            0 => (op & 0xfff) as u32,
            1 => self.ar[rx],
            2 => {
                if op & (1 << 11) != 0 {
                    self.ab[rx]
                } else {
                    self.aa[rx]
                }
            }
            3 => {
                if op & (1 << 11) != 0 {
                    self.mb[rx]
                } else {
                    self.ma[rx]
                }
            }
            4 => {
                let a = (op & 0x3ff) as u32;
                self.read_abus(bus, a)
            }
            5 => {
                let a = (op & 0x3ff) as u32;
                self.read_bbus(a)
            }
            6 => {
                let a = self.ar[rx];
                self.read_abus(bus, a)
            }
            _ => {
                let a = self.ar[rx];
                self.read_bbus(a)
            }
        };
        if op & (1 << 12) != 0 {
            self.icount -= 1;
            self.pc.wrapping_add(addr) & 0xfff
        } else {
            addr & 0xfff
        }
    }

    #[inline]
    fn set_mod(&mut self, clear: u32, set: u32) {
        self.mod_ &= !clear;
        self.mod_ |= set;
    }

    /// The control class: branches, calls, loops and the mode registers.
    ///
    fn do_control<B: Mb86235Bus>(&mut self, bus: &mut B, op: u64) {
        use crate::state::flag::*;
        let cop = ((op >> 22) & 0x1f) as u32;
        let ef1 = ((op >> 16) & 0x3f) as u32;
        let ef2 = (op & 0xffff) as u32;

        match cop {
            0x00 => {} // NOP
            0x01 => {
                // REP
                self.rpc = if ef1 == 0x3f {
                    self.ar[((ef2 >> 13) & 7) as usize]
                } else {
                    ef2
                };
                self.st |= RP;
            }
            0x02 => {
                // SETL
                self.lpc = if ef1 == 0x3f {
                    self.ar[((ef2 >> 13) & 7) as usize]
                } else {
                    ef2
                };
                self.st |= LP;
            }
            0x03 => {} // CLRF: the board's FIFOs are cleared by the host side
            0x04 => {
                // PUSH
                self.sp = self.sp.wrapping_sub(1) & 0x3ff;
                let v = self.get_transfer_reg(bus, ((ef2 >> 6) & 0x3f) as u8);
                let sp = self.sp;
                self.write_bbus(sp, v);
            }
            0x05 => {} // POP: deferred until after the ALU, below
            0x08 => self.set_mod(0xffff, ef2),
            0x09 => self.set_mod(0x7000, ef2),
            0x0a => self.set_mod(0x0e00, ef2),
            0x0b => self.set_mod(0x0080, ef2),
            0x0c => self.set_mod(0x0010, ef2),
            0x0d => self.set_mod(0x0007, ef2),

            // --- control flow; every branch here has one delay slot ---
            0x10 | 0x11 => {
                // DBcc / DBNcc
                let taken = self.branch_cond(bus, ef1) == (cop == 0x10);
                if taken {
                    self.delay_slot = true;
                    self.delay_pc = self.control_dst(bus, op);
                }
                self.icount -= 1;
            }
            0x12 => {
                // DJMP
                self.delay_slot = true;
                self.delay_pc = self.control_dst(bus, op);
                self.icount -= 1;
            }
            0x13 => {
                // DBLP: loop back while the loop counter is live
                if self.st & LP != 0 {
                    self.delay_slot = true;
                    self.delay_pc = self.pc.wrapping_add((op & 0xfff) as u32) & 0xfff;
                }
                self.lpc = self.lpc.wrapping_sub(1);
                if self.lpc == 1 {
                    self.st &= !LP;
                }
            }
            0x14 | 0x15 => {
                // DBBC / DBBS: branch on an AR bit
                let bit = self.ar[((op >> 13) & 7) as usize] & (1 << ((op >> 16) & 0xf)) != 0;
                if bit == (cop == 0x15) {
                    self.delay_slot = true;
                    self.delay_pc = self.pc.wrapping_add((op & 0xfff) as u32) & 0xfff;
                }
                self.icount -= 2;
            }
            0x18 | 0x19 => {
                // DCcc / DCNcc
                let taken = self.branch_cond(bus, ef1) == (cop == 0x18);
                if taken {
                    self.delay_slot = true;
                    self.delay_pc = self.control_dst(bus, op);
                    let ret = self.pc.wrapping_add(1);
                    self.push_pc(ret);
                }
                self.icount -= 1;
            }
            0x1a => {
                // DCALL
                self.delay_slot = true;
                self.delay_pc = self.control_dst(bus, op);
                let ret = self.pc.wrapping_add(1);
                self.push_pc(ret);
            }
            0x1b => {
                // DRET
                self.delay_slot = true;
                self.delay_pc = self.pop_pc();
            }
            _ => self.unimpl[2] += 1,
        }

        // The ALU slot issues alongside the control operation.
        if op & (1u64 << 63) != 0 {
            self.do_alu1(bus, op);
        } else {
            self.do_alu2(bus, op);
        }

        // POP reads the stack only once the ALU has settled.
        if cop == 0x05 {
            let sp = self.sp;
            let v = self.read_bbus(sp);
            self.set_transfer_reg(bus, ((ef2 >> 6) & 0x3f) as u8, v);
            self.sp = self.sp.wrapping_add(1) & 0x3ff;
        }
    }

    fn do_alu1<B: Mb86235Bus>(&mut self, _bus: &mut B, op: u64) {
        self.do_alu1_op(op);
    }
    fn do_alu2<B: Mb86235Bus>(&mut self, _bus: &mut B, op: u64) {
        self.do_alu2_op(op);
    }
}
