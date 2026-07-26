use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, SP};

impl I960Cpu {
    pub fn op_sys<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;
        match op_idx {
            0x08 => {
                self.icount -= 1;
                self.ip = self.ip.wrapping_add(self.get_disp(opcode));
            }
            0x09 => {
                let dest = self.ip.wrapping_add(self.get_disp(opcode));
                let sp = self.r[SP];
                // Calls the shared do_call implementation in core.rs
                self.do_call(bus, dest, 0, sp);
            }
            0x0a => {
                // Calls the shared do_ret implementation in core.rs
                self.do_ret(bus);
            }
            0x0b => {
                self.icount -= 5;
                self.r[0x1e] = self.ip; // g14
                self.ip = self.ip.wrapping_add(self.get_disp(opcode));
            }

            // Branch Condition
            0x10 => {
                self.icount -= 1;
                if (self.ac & 7) == 0 {
                    self.ip = self.ip.wrapping_add(self.get_disp(opcode));
                }
            }
            0x11 => {
                self.icount -= 1;
                self.bxx(opcode, 1);
            }
            0x12 => {
                self.icount -= 1;
                self.bxx(opcode, 2);
            }
            0x13 => {
                self.icount -= 1;
                self.bxx(opcode, 3);
            }
            0x14 => {
                self.icount -= 1;
                self.bxx(opcode, 4);
            }
            0x15 => {
                self.icount -= 1;
                self.bxx(opcode, 5);
            }
            0x16 => {
                self.icount -= 1;
                self.bxx(opcode, 6);
            }
            0x17 => {
                self.icount -= 1;
                self.bxx(opcode, 7);
            }

            // --- Compare and Branch (COBR format) ---
            0x30 | 0x37 => {
                // bbc / bbs
                self.icount -= 4;
                let bit = self.get_1_ci(opcode) & 0x1f;
                let src = self.get_2_ci(opcode);
                let is_set = (src & (1 << bit)) != 0;
                // bbc branches when the bit is clear, bbs when it is set.
                if is_set == (op_idx == 0x37) {
                    self.ac = (self.ac & !7) | 2;
                    self.ip = self.ip.wrapping_add(self.get_disp_s(opcode)) & !3;
                } else {
                    self.ac &= !7;
                }
            }
            0x31..=0x36 => {
                // cmpob* (unsigned)
                self.icount -= 4;
                let t1 = self.get_1_ci(opcode);
                let t2 = self.get_2_ci(opcode);
                self.cmp_u(t1, t2);
                let mask = op_idx - 0x30;
                if (self.ac & mask) != 0 {
                    self.ip = self.ip.wrapping_add(self.get_disp_s(opcode)) & !3;
                }
            }
            0x39..=0x3e => {
                // cmpib* (signed)
                self.icount -= 4;
                let t1 = self.get_1_ci(opcode);
                let t2 = self.get_2_ci(opcode);
                self.cmp_s(t1, t2);
                let mask = op_idx - 0x38;
                if (self.ac & mask) != 0 {
                    self.ip = self.ip.wrapping_add(self.get_disp_s(opcode)) & !3;
                }
            }
            _ => {}
        }
    }
}
