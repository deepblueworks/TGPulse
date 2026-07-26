use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, SP};

impl I960Cpu {
    pub fn op_sys_complex<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;
        let sub = (opcode >> 7) & 0xf;

        match op_idx {
            // 0x61: Atomic Operations (Crucial for multiprocessing/OS)
            0x61 => match sub {
                0x0 => {
                    // atmod (Atomic Modify)
                    self.icount -= 33; // Approx cycles
                    let addr = self.get_1_ri(opcode);
                    let mask = self.get_2_ri(opcode);
                    let src_dst_reg = ((opcode >> 19) & 0x1f) as usize;
                    let val = self.r[src_dst_reg];

                    let old_val = bus.read_u32(addr);
                    let new_val = (old_val & !mask) | (val & mask);
                    bus.write_u32(addr, new_val);
                    self.r[src_dst_reg] = old_val;
                }
                0x2 => {
                    // atadd (Atomic Add)
                    self.icount -= 33;
                    let addr = self.get_1_ri(opcode);
                    let src = self.get_2_ri(opcode);
                    let src_dst_reg = ((opcode >> 19) & 0x1f) as usize;

                    let old_val = bus.read_u32(addr);
                    let new_val = old_val.wrapping_add(src);
                    bus.write_u32(addr, new_val);
                    self.r[src_dst_reg] = old_val;
                }
                _ => {}
            },

            // 0x64: Bit Scanning & Decimal
            0x64 => match sub {
                0x0 => {
                    // spanbit (Find first bit 0)
                    self.icount -= 10;
                    let src = self.get_1_ri(opcode);
                    self.ac &= !7; // Clear condition codes
                    let mut res = 0xffffffff;

                    for i in (0..32).rev() {
                        if (src & (1 << i)) == 0 {
                            self.ac |= 2; // Set Condition Code "Equal" (010)
                            res = i as u32;
                            break;
                        }
                    }
                    self.set_ri(opcode, res);
                }
                0x1 => {
                    // scanbit (Find first bit 1)
                    self.icount -= 10;
                    let src = self.get_1_ri(opcode);
                    self.ac &= !7;
                    let mut res = 0xffffffff;

                    for i in (0..32).rev() {
                        if (src & (1 << i)) != 0 {
                            self.ac |= 2;
                            res = i as u32;
                            break;
                        }
                    }
                    self.set_ri(opcode, res);
                }
                0x4 => {
                    // dmovt (Decimal Move and Test)
                    self.icount -= 7;
                    let src = self.get_1_ri(opcode);
                    self.set_ri(opcode, src);
                    self.ac &= 0xfff8;
                    // Check if byte is valid ASCII digit '0'-'9' (0x30-0x39)
                    let byte = src & 0xff;
                    if !(0x30..=0x39).contains(&byte) {
                        self.ac |= 2;
                    }
                }
                0x5 => {
                    // modac (Modify AC)
                    self.icount -= 10;
                    let mask = self.get_1_ri(opcode);
                    let src = self.get_2_ri(opcode);
                    self.set_ri(opcode, self.ac); // Save old AC
                    self.ac = (self.ac & !mask) | (src & mask);
                }
                _ => {}
            },

            // 0x65: Process Controls
            0x65 => {
                if sub == 0x5 {
                    // modpc (Modify Process Controls)
                    self.icount -= 10;
                    let mask = self.get_2_ri(opcode);
                    let src = self.r[((opcode >> 19) & 0x1f) as usize];
                    let old_pc = self.pc;

                    self.pc = (self.pc & !mask) | (src & mask);
                    self.set_ri(opcode, old_pc);

                    // If priority changed, check IRQs (Placeholder for next step)
                    // if (old_pc >> 16 & 0x1f) > (self.pc >> 16 & 0x1f) { check_pending_irqs(); }
                }
            }

            // 0x66: System Calls & Management
            0x66 => match sub {
                0x0 => {
                    // calls (System Call)
                    // 1. Get index from operand
                    let idx = self.get_1_ri(opcode);
                    // 2. Read System Procedure Table pointer from SAT + 152
                    let spt_base = bus.read_u32(self.sat + 152);
                    // 3. Read entry from table (Entry = 48 + index * 4)
                    let target_addr = bus.read_u32(spt_base + 48 + (idx * 4));

                    // Note: Real HW checks bottom 2 bits for Supervisor call type.
                    // The reference generally ignores this or panics if != 0.
                    self.do_call(bus, target_addr & !3, 0, self.r[SP]);
                }
                0xd => {
                    // flushreg (Flush Register Cache)
                    // Forces all cached register windows out to stack memory.
                    // Essential before context switching.

                    // Clamp to max physical cache size (4 frames)
                    let mut limit = self.rcache_pos;
                    if limit > 4 {
                        limit = 4;
                    }

                    for i in 0..limit {
                        let frame_ptr = self.rcache_frame_addr[i as usize];
                        for reg in 0..16 {
                            bus.write_u32(
                                frame_ptr + (reg as u32 * 4),
                                self.rcache[i as usize][reg as usize],
                            );
                        }
                    }
                    self.rcache_pos = 0;
                }
                _ => {}
            },

            _ => {}
        }
    }
}
