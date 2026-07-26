use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, SP};

impl I960Cpu {
    pub fn op_mem<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;
        match op_idx {
            // 0x80: Load Byte / Store Byte
            0x80 => {
                self.icount -= 4;
                let ea = self.get_ea(bus, opcode);
                let v = bus.read_byte(ea);
                self.set_ri(opcode, v as u32);
            } // ldob
            0x82 => {
                self.icount -= 2;
                let ea = self.get_ea(bus, opcode);
                let v = self.r[((opcode >> 19) & 0x1f) as usize];
                bus.write_byte(ea, v as u8);
            } // stob

            // 0x88/0x8A: Load/Store Ordinal Short (16-bit, unsigned)
            0x88 => {
                self.icount -= 4;
                let ea = self.get_ea(bus, opcode);
                let v = bus.read_u16(ea);
                if !self.stalled {
                    self.set_ri(opcode, v as u32);
                }
            } // ldos
            0x8a => {
                self.icount -= 2;
                let ea = self.get_ea(bus, opcode);
                let v = self.r[((opcode >> 19) & 0x1f) as usize];
                bus.write_u16(ea, v as u16);
            } // stos

            // 0x84: Branches
            0x84 => {
                self.icount -= 3;
                self.ip = self.get_ea(bus, opcode);
            } // bx
            0x85 => {
                self.icount -= 5;
                let t1 = self.get_ea(bus, opcode);
                self.r[((opcode >> 19) & 0x1f) as usize] = self.ip;
                self.ip = t1;
            } // balx
            0x86 => {
                let t1 = self.get_ea(bus, opcode);
                let sp = self.r[SP];
                self.do_call(bus, t1, 0, sp);
            } // callx

            // 0x8C: Load Address
            0x8c => {
                self.icount -= 1;
                let ea = self.get_ea(bus, opcode);
                self.set_ri(opcode, ea);
            } // lda

            // 0x90: Load / Store (32-bit)
            0x90 => {
                self.icount -= 4;
                let ea = self.get_ea(bus, opcode);
                let v = bus.read_u32(ea);
                if !self.stalled {
                    self.set_ri(opcode, v);
                }
            }
            0x92 => {
                self.icount -= 2;
                let ea = self.get_ea(bus, opcode);
                let v = self.r[((opcode >> 19) & 0x1f) as usize];
                bus.write_u32(ea, v);
            }

            // --- BURST INSTRUCTIONS (Long, Triple, Quad) ---

            // 0x98: ldl (Load Long - 2 words)
            0x98 => {
                self.icount -= 5;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1e) as usize; // Must be even register

                for i in 0..2 {
                    let v = bus.read_u32(ea);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 2, false);
                        return;
                    }
                    self.r[t2 + i] = v;
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0x9A: stl (Store Long - 2 words)
            0x9a => {
                self.icount -= 3;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1e) as usize;

                for i in 0..2 {
                    let v = self.r[t2 + i];
                    bus.write_u32(ea, v);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 2, true);
                        return;
                    }
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0xA0: ldt (Load Triple - 3 words)
            0xa0 => {
                self.icount -= 6;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1c) as usize; // Must be aligned to 4

                for i in 0..3 {
                    let v = bus.read_u32(ea);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 3, false);
                        return;
                    }
                    self.r[t2 + i] = v;
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0xA2: stt (Store Triple - 3 words)
            0xa2 => {
                self.icount -= 4;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1c) as usize;

                for i in 0..3 {
                    let v = self.r[t2 + i];
                    bus.write_u32(ea, v);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 3, true);
                        return;
                    }
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0xB0: ldq (Load Quad - 4 words)
            0xb0 => {
                self.icount -= 7;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1c) as usize;

                for i in 0..4 {
                    let v = bus.read_u32(ea);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 4, false);
                        return;
                    }
                    self.r[t2 + i] = v;
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0xB2: stq (Store Quad - 4 words)
            0xb2 => {
                self.icount -= 5;
                let mut ea = self.get_ea(bus, opcode);
                let t2 = ((opcode >> 19) & 0x1c) as usize;

                for i in 0..4 {
                    let v = self.r[t2 + i];
                    bus.write_u32(ea, v);
                    self.stalled |= bus.take_stall();
                    if self.stalled {
                        self.burst_stall_save(ea, t2, i, 4, true);
                        return;
                    }
                    if bus.burst_capable(ea) {
                        ea = ea.wrapping_add(4);
                    }
                }
            }

            // 0xC0: Load/Store Integer Byte/Short (Signed)
            0xc0 => {
                self.icount -= 4;
                let ea = self.get_ea(bus, opcode);
                let v = bus.read_byte(ea) as i8;
                self.set_ri(opcode, v as i32 as u32);
            } // ldib
            0xc2 => {
                self.icount -= 2;
                let ea = self.get_ea(bus, opcode);
                let v = self.r[((opcode >> 19) & 0x1f) as usize];
                bus.write_byte(ea, v as u8);
            } // stib
            0xc8 => {
                self.icount -= 4;
                let ea = self.get_ea(bus, opcode);
                let v = bus.read_u16(ea) as i16;
                self.set_ri(opcode, v as i32 as u32);
            } // ldis
            0xca => {
                self.icount -= 2;
                let ea = self.get_ea(bus, opcode);
                let v = self.r[((opcode >> 19) & 0x1f) as usize];
                bus.write_u16(ea, v as u16);
            } // stis

            _ => {}
        }
    }
}
