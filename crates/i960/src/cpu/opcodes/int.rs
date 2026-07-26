//! REG-format integer, logic, move and compare instructions. Opcode/sub-opcode
//! assignments follow the decoder; the sub-opcode is bits 10-7.

use crate::bus::Bus;
use crate::cpu::defs::{I960Cpu, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE};

impl I960Cpu {
    pub fn op_int<B: Bus>(&mut self, bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;
        let sub = (opcode >> 7) & 0xf;

        match op_idx {
            // --- 0x20-0x27: test<cc>, store the condition code as 0/1 ---
            0x20..=0x27 => {
                self.icount -= 1;
                let val = if op_idx == 0x20 {
                    // testno: true when no condition bits are set
                    u32::from((self.ac & 7) == 0)
                } else {
                    u32::from((self.ac & (op_idx - 0x20)) != 0)
                };
                self.r[((opcode >> 19) & 0x1f) as usize] = val;
            }

            // --- 0x58: bitwise logic ---
            0x58 => {
                self.icount -= if matches!(sub, 0x0 | 0x3 | 0xc | 0xf) {
                    2
                } else {
                    1
                };
                let t1 = self.get_1_ri(opcode);
                let t2 = self.get_2_ri(opcode);
                let bit = 1u32 << (t1 & 31);
                let res = match sub {
                    0x0 => t2 ^ bit,   // notbit
                    0x1 => t2 & t1,    // and
                    0x2 => t2 & !t1,   // andnot
                    0x3 => t2 | bit,   // setbit
                    0x4 => !t2 & t1,   // notand
                    0x6 => t2 ^ t1,    // xor
                    0x7 => t2 | t1,    // or
                    0x8 => !t2 & !t1,  // nor
                    0x9 => !(t2 ^ t1), // xnor
                    0xa => !t1,        // not
                    0xb => t2 | !t1,   // ornot
                    0xc => t2 & !bit,  // clrbit
                    0xd => !t2 | t1,   // notor
                    0xe => !t2 | !t1,  // nand
                    0xf => {
                        // alterbit
                        if self.ac & 2 != 0 {
                            t2 | bit
                        } else {
                            t2 & !bit
                        }
                    }
                    _ => return,
                };
                self.set_ri(opcode, res);
            }

            // --- 0x59: add / subtract / shift ---
            0x59 => {
                self.icount -= 1;
                let t1 = self.get_1_ri(opcode);
                let t2 = self.get_2_ri(opcode);
                let res = match sub {
                    // addo / addi (integer overflow is not modelled)
                    0x0 | 0x1 => t2.wrapping_add(t1),
                    // subo / subi
                    0x2 | 0x3 => t2.wrapping_sub(t1),
                    0x8 => {
                        if t1 >= 32 {
                            0
                        } else {
                            t2 >> t1
                        }
                    } // shro
                    0xa => {
                        // shrdi
                        if t1 >= 32 {
                            0
                        } else if (t2 as i32) < 0 && (t2 & ((1 << t1) - 1)) != 0 {
                            // Round toward zero for negative values
                            (((t2 as i32) >> t1) + 1) as u32
                        } else {
                            ((t2 as i32) >> t1) as u32
                        }
                    }
                    0xb => {
                        // shri
                        if t1 >= 32 {
                            if (t2 as i32) < 0 {
                                u32::MAX
                            } else {
                                0
                            }
                        } else {
                            ((t2 as i32) >> t1) as u32
                        }
                    }
                    // shlo / shli (overflow is not modelled)
                    0xc | 0xe => {
                        if t1 >= 32 {
                            0
                        } else {
                            t2 << t1
                        }
                    }
                    0xd => t2.rotate_left(t1 & 0x1f), // rotate
                    _ => return,
                };
                self.set_ri(opcode, res);
            }

            // --- 0x5A: compare ---
            0x5A => match sub {
                0x0 => {
                    // cmpo
                    self.icount -= 1;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_u(t1, t2);
                }
                0x1 => {
                    // cmpi
                    self.icount -= 1;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_s(t1, t2);
                }
                0x2 => {
                    // concmpo: only compares when the carry bit is clear
                    self.icount -= 1;
                    if self.ac & 4 == 0 {
                        let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                        self.concmp_u(t1, t2);
                    }
                }
                0x3 => {
                    // concmpi
                    self.icount -= 1;
                    if self.ac & 4 == 0 {
                        let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                        self.concmp_s(t1, t2);
                    }
                }
                0x4 => {
                    // cmpinco
                    self.icount -= 2;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_u(t1, t2);
                    self.set_ri(opcode, t2.wrapping_add(1));
                }
                0x5 => {
                    // cmpinci
                    self.icount -= 2;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_s(t1, t2);
                    self.set_ri(opcode, t2.wrapping_add(1));
                }
                0x6 => {
                    // cmpdeco
                    self.icount -= 2;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_u(t1, t2);
                    self.set_ri(opcode, t2.wrapping_sub(1));
                }
                0x7 => {
                    // cmpdeci
                    self.icount -= 2;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.cmp_s(t1, t2);
                    self.set_ri(opcode, t2.wrapping_sub(1));
                }
                0xc => {
                    // scanbyte: sets the equal flag if any byte lane matches
                    self.icount -= 2;
                    let (t1, t2) = (self.get_1_ri(opcode), self.get_2_ri(opcode));
                    self.ac &= !7;
                    let hit = (0..4).any(|i| {
                        let m = 0xFFu32 << (i * 8);
                        (t1 & m) == (t2 & m)
                    });
                    if hit {
                        self.ac |= 2;
                    }
                }
                0xe => {
                    // chkbit
                    self.icount -= 2;
                    let t1 = self.get_1_ri(opcode) & 0x1f;
                    let t2 = self.get_2_ri(opcode);
                    if t2 & (1 << t1) != 0 {
                        self.ac = (self.ac & !7) | 2;
                    } else {
                        self.ac &= !7;
                    }
                }
                _ => {}
            },

            // --- 0x5B: add/subtract with carry ---
            0x5B => match sub {
                0x0 => {
                    // addc
                    self.icount -= 1;
                    let t1 = self.get_1_ri(opcode);
                    let t2 = self.get_2_ri(opcode);
                    let res = t2 as u64 + t1 as u64 + ((self.ac >> 1) & 1) as u64;
                    self.set_ri(opcode, res as u32);
                    self.ac &= !3;
                    if res & (1 << 32) != 0 {
                        self.ac |= 2; // carry
                    }
                    if ((res as u32) ^ t1) & ((res as u32) ^ t2) & 0x8000_0000 != 0 {
                        self.ac |= 1; // overflow
                    }
                }
                0x2 => {
                    // subc
                    self.icount -= 1;
                    let t1 = self.get_1_ri(opcode);
                    let t2 = self.get_2_ri(opcode);
                    let res = (t2 as u64).wrapping_sub(t1 as u64 + ((self.ac >> 1) & 1) as u64);
                    self.set_ri(opcode, res as u32);
                    self.ac &= !3;
                    if res & (1 << 32) != 0 {
                        self.ac |= 2; // carry
                    }
                    if (t2 ^ t1) & (t2 ^ (res as u32)) & 0x8000_0000 != 0 {
                        self.ac |= 1; // overflow
                    }
                }
                _ => {}
            },

            // --- 0x5C-0x5F: mov / movl / movt / movq ---
            0x5C => {
                if sub == 0xc {
                    self.icount -= 2;
                    let t1 = self.get_1_ri(opcode);
                    self.set_ri(opcode, t1);
                }
            }
            0x5D..=0x5F => {
                if sub == 0xc {
                    self.icount -= 2;
                    // movl/movt/movq move 2/3/4 registers; the destination is
                    // aligned to that width.
                    let (count, align) = match op_idx {
                        0x5D => (2usize, 0x1eusize),
                        0x5E => (3, 0x1c),
                        _ => (4, 0x1c),
                    };
                    let dst = ((opcode >> 19) as usize) & align;
                    if opcode & 0x800 != 0 {
                        // Literal: every destination register gets the same value
                        let lit = opcode & 0x1f;
                        for i in 0..count {
                            self.r[dst + i] = lit;
                        }
                    } else {
                        let src = (opcode & 0x1f) as usize;
                        for i in 0..count {
                            self.r[dst + i] = self.r[src + i];
                        }
                    }
                }
            }

            // --- 0x60: synmov / synmovq ---
            // src1 is the destination address, src2 the source address. Two
            // addresses are special-cased as IAC (Inter-Agent Communication)
            // ports rather than memory.
            0x60 => match sub {
                0x0 => {
                    // synmov
                    self.icount -= 6;
                    let dst = self.get_1_ri(opcode);
                    let src = self.get_2_ri(opcode);
                    if dst == 0xFF00_0004 {
                        // Interrupt control register
                        self.icr = bus.read_u32(src);
                    } else {
                        let val = bus.read_u32(src);
                        bus.write_u32(dst, val);
                    }
                    self.ac = (self.ac & !7) | 2;
                }
                0x2 => {
                    // synmovq
                    self.icount -= 12;
                    let dst = self.get_1_ri(opcode);
                    let src = self.get_2_ri(opcode);
                    if dst == 0xFF00_0010 {
                        self.send_iac(bus, src);
                    } else {
                        for i in 0..4 {
                            let val = bus.read_u32(src.wrapping_add(i * 4));
                            bus.write_u32(dst.wrapping_add(i * 4), val);
                        }
                    }
                    self.ac = (self.ac & !7) | 2;
                }
                _ => {}
            },

            // --- 0x70: unsigned multiply / remainder / divide ---
            0x70 => {
                let t1 = self.get_1_ri(opcode);
                let t2 = self.get_2_ri(opcode);
                match sub {
                    0x1 => {
                        // mulo
                        self.icount -= 18;
                        self.set_ri(opcode, t2.wrapping_mul(t1));
                    }
                    0x8 => {
                        // remo
                        self.icount -= 37;
                        if t1 == 0 {
                            self.generate_fault(bus, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE);
                        } else {
                            self.set_ri(opcode, t2 % t1);
                        }
                    }
                    0xb => {
                        // divo
                        self.icount -= 37;
                        match t2.checked_div(t1) {
                            Some(q) => self.set_ri(opcode, q),
                            None => self.generate_fault(bus, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE),
                        }
                    }
                    _ => {}
                }
            }

            // --- 0x74: signed multiply / remainder / modulo / divide ---
            0x74 => {
                let t1 = self.get_1_ri(opcode) as i32;
                let t2 = self.get_2_ri(opcode) as i32;
                match sub {
                    0x1 => {
                        // muli
                        self.icount -= 18;
                        self.set_ri(opcode, t2.wrapping_mul(t1) as u32);
                    }
                    0x8 => {
                        // remi
                        self.icount -= 37;
                        if t1 == 0 {
                            self.generate_fault(bus, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE);
                        } else {
                            self.set_ri(opcode, t2.wrapping_rem(t1) as u32);
                        }
                    }
                    0x9 => {
                        // modi
                        self.icount -= 37;
                        if t1 == 0 {
                            self.generate_fault(bus, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE);
                        } else {
                            let mut dst = t2.wrapping_sub(t2.wrapping_div(t1).wrapping_mul(t1));
                            if t2.wrapping_mul(t1) < 0 && dst != 0 {
                                dst = dst.wrapping_add(t1);
                            }
                            self.set_ri(opcode, dst as u32);
                        }
                    }
                    0xb => {
                        // divi
                        self.icount -= 37;
                        if t1 == 0 {
                            self.generate_fault(bus, FAULT_ARITHMETIC, FSUB_ZERO_DIVIDE);
                        } else {
                            self.set_ri(opcode, t2.wrapping_div(t1) as u32);
                        }
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }
}
