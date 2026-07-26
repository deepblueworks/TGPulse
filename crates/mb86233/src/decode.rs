use crate::cpu_state::Mb86233;
use crate::memory::Mb86233Bus;
use crate::types::*;

impl Mb86233 {
    /// Data-space read that honors a bus-side stall request (an empty input
    /// FIFO pop). The register-file path does this inside `read_reg`; the
    /// data-space FIFO port needs the same treatment, otherwise an empty pop
    /// returns a bogus 0 and the microcode dispatches a phantom command
    /// (Model 1 reads its command FIFO through data space, unlike Model 2).
    fn read_data_stall(&mut self, bus: &mut impl Mb86233Bus, ea: u32) -> u32 {
        let v = bus.read_data(ea);
        if bus.take_stall() {
            self.stall = true;
        }
        v
    }

    pub fn execute(&mut self, bus: &mut impl Mb86233Bus, cycles: i32) {
        self.icount = cycles;

        while self.icount > 0 {
            self.ppc = self.pc;
            let opcode = bus.read_program(self.pc as u32);
            self.pc = self.pc.wrapping_add(1);

            // TGP_PCLOG=<file> records the whole executed PC stream, in the
            // same "%04X:" form the reference tracer emits, so the two cores can be
            // diffed instruction for instruction.

            // Flag to prevent REP instruction from repeating itself
            let mut is_rep = false;

            // On a stalled access the PC is rewound and the instruction is
            // abandoned for the rest of this quantum. The only port that stalls
            // is the input FIFO, and only the main CPU refills it -- which
            // cannot happen while the TGP holds the scheduler. Re-trying the
            // access here would therefore fail every time until the quantum
            // expired
            // instead of spinning.

            let group = (opcode >> 26) & 0x3f;

            match group {
                0x00 => {
                    // lab (Load A/B)
                    let r1 = opcode & 0x1ff;
                    let r2 = (opcode >> 9) & 0x1ff;
                    let alu = (opcode >> 21) & 0x1f;
                    let op = (opcode >> 18) & 0x7;

                    self.alu_pre(alu);

                    match op {
                        0 | 1 => {
                            let ea1 = self.ea_pre_0(r1) as u32;
                            let v1 = self.read_data_stall(bus, ea1);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            let ea2 = self.ea_pre_1(r2) as u32;
                            let v2 = bus.read_io(ea2);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            self.ea_post_0(r1);
                            self.ea_post_1(r2);
                            self.a = v1;
                            self.b = v2;
                        }
                        3 => {
                            let ea1 = self.ea_pre_0(r1) as u32;
                            let v1 = self.read_data_stall(bus, ea1);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            let ea2 = (self.ea_pre_1(r2) as u32).wrapping_add(0x200);
                            let v2 = self.read_data_stall(bus, ea2);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            self.ea_post_0(r1);
                            self.ea_post_1(r2);
                            self.a = v1;
                            self.b = v2;
                        }
                        4 => {
                            let ea1 = (self.ea_pre_0(r1) as u32).wrapping_add(0x200);
                            let v1 = self.read_data_stall(bus, ea1);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            let ea2 = self.ea_pre_1(r2) as u32;
                            let v2 = self.read_data_stall(bus, ea2);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }

                            self.ea_post_0(r1);
                            self.ea_post_1(r2);
                            self.a = v1;
                            self.b = v2;
                        }
                        _ => {}
                    }
                    self.alu_post_1(alu);
                    self.alu_post_2(alu);
                }

                0x07 => {
                    // ld / mov
                    let r1 = opcode & 0x1ff;
                    let r2 = (opcode >> 9) & 0x1ff;
                    let alu = (opcode >> 21) & 0x1f;
                    let op = (opcode >> 18) & 0x7;

                    self.alu_pre(alu);

                    match op {
                        0 | 1 => {
                            let ea = self.ea_pre_0(r1) as u32;
                            let v = self.read_data_stall(bus, ea);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.ea_post_0(r1);
                            self.alu_post_1(alu);
                            self.write_mem_io_1(bus, r2, v);
                        }
                        2 => {
                            let ea = self.ea_pre_0(r1) as u32;
                            let v = bus.read_io(ea);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.ea_post_0(r1);
                            self.alu_post_1(alu);
                            self.write_mem_internal_1(bus, r2, v, false);
                        }
                        3 => {
                            let ea = self.ea_pre_0(r1) as u32;
                            let v = self.read_data_stall(bus, ea);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.ea_post_0(r1);
                            self.alu_post_1(alu);
                            self.write_mem_internal_1(bus, r2, v, true);
                        }
                        4 => {
                            let ea = (self.ea_pre_0(r1) as u32).wrapping_add(0x200);
                            let v = self.read_data_stall(bus, ea);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.ea_post_0(r1);
                            self.alu_post_1(alu);
                            self.write_mem_internal_1(bus, r2, v, false);
                        }
                        5 => {
                            let ea = self.ea_pre_0(r1) as u32;
                            let v = bus.read_program(ea);
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.ea_post_0(r1);
                            self.alu_post_1(alu);
                            self.write_mem_internal_1(bus, r2, v, false);
                        }
                        7 => match r2 >> 6 {
                            0 => {
                                let v = self.read_reg(bus, r2);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.alu_post_1(alu);
                                self.write_mem_internal_1(bus, r1, v, false);
                            }
                            1 => {
                                let v = self.read_reg(bus, r2);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.alu_post_1(alu);
                                self.write_mem_io_1(bus, r1, v);
                            }
                            2 => {
                                let ea = (self.ea_pre_1(r1) as u32).wrapping_add(0x200);
                                let v = self.read_data_stall(bus, ea);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.ea_post_1(r1);
                                self.alu_post_1(alu);
                                self.write_reg(bus, r2, v);
                            }
                            3 => {
                                let ea = self.ea_pre_1(r1) as u32;
                                let v = self.read_data_stall(bus, ea);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.ea_post_1(r1);
                                self.alu_post_1(alu);
                                self.write_reg(bus, r2, v);
                            }
                            4 => {
                                let ea = self.ea_pre_1(r1) as u32;
                                let v = bus.read_io(ea);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.ea_post_1(r1);
                                self.alu_post_1(alu);
                                self.write_reg(bus, r2, v);
                            }
                            5 => {
                                let ea = self.ea_pre_0(r1) as u32;
                                let v = bus.read_program(ea);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.ea_post_0(r1);
                                self.alu_post_1(alu);
                                self.write_reg(bus, r2, v);
                            }
                            6 => {
                                let v = self.read_reg(bus, r1);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.alu_post_1(alu);
                                self.write_reg(bus, r2, v);
                            }
                            _ => {
                                self.alu_post_1(alu);
                            }
                        },
                        _ => {
                            self.alu_post_1(alu);
                        }
                    }
                    self.alu_post_2(alu);
                }

                0x0D => {
                    let sub2 = (opcode >> 17) & 7;
                    if sub2 == 5 {
                        self.m = opcode as u16;
                    }
                }

                0x0E => {
                    let val = opcode & 0xffffff;
                    match (opcode >> 24) & 0x3 {
                        0 => self.p = (self.p & 0xff000000) | val,
                        1 => self.a = sext(val, 24),
                        2 => self.b = sext(val, 24),
                        3 => self.d = sext(val, 24),
                        _ => {}
                    }
                }

                0x0F => {
                    let alu = (opcode >> 20) & 0x1f;
                    let sub2 = (opcode >> 17) & 7;

                    self.alu_pre(alu);

                    match sub2 {
                        0 => {
                            if (opcode & 0x0004) != 0 {
                                self.a = 0;
                            }
                            if (opcode & 0x0008) != 0 {
                                self.b = 0;
                            }
                            if (opcode & 0x0010) != 0 {
                                self.d = 0;
                            }
                        }
                        2 => {
                            // rep
                            let r_val = if (opcode & 0x8000) != 0 {
                                self.read_reg(bus, opcode)
                            } else {
                                // The reference: if (opcode & 0xff == 0) r = 0x100
                                let imm = opcode & 0xff;
                                if imm == 0 {
                                    0x100
                                } else {
                                    imm
                                }
                            };
                            if self.stall {
                                self.pc = self.ppc;
                                self.stall = false;
                                self.cov.stall_retries += 1;
                                self.icount -= 1;
                                break;
                            }
                            self.r = r_val as u8;
                            is_rep = true; // <--- FIX: Signal to skip repetition logic for this cycle
                        }
                        3 => {}
                        _ => {}
                    }
                    self.alu_post_1(alu);
                }

                0x10..=0x1F => {
                    self.write_reg(bus, opcode >> 24, sext(opcode, 24));
                }

                0x2F | 0x3F => {
                    let cond = (opcode >> 20) & 0x1f;
                    let subtype = (opcode >> 17) & 7;
                    let data = (opcode & 0xffff) as u16;
                    let invert = (opcode & 0x40000000) != 0;

                    let mut cond_passed = match cond {
                        0x00 => (self.st & F_ZRD) != 0,
                        0x01 => (self.st & F_SGD) == 0,
                        0x02 => (self.st & (F_ZRD | F_SGD)) != 0,
                        0x0A => bus.gpio(0),
                        0x0B => bus.gpio(1),
                        0x0C => bus.gpio(2),
                        0x10 => (self.st & F_ZC0) == 0,
                        0x11 => (self.st & F_ZC1) == 0,
                        0x12 => bus.gpio(3),
                        0x16 => true,
                        _ => false,
                    };

                    if invert {
                        cond_passed = !cond_passed;
                    }

                    if cond_passed {
                        match subtype {
                            0 => {
                                self.pc = data;
                            }
                            1 => {
                                if (opcode & 0x4000) != 0 {
                                    let v = self.read_reg(bus, opcode);
                                    if self.stall {
                                        self.pc = self.ppc;
                                        self.stall = false;
                                        self.cov.stall_retries += 1;
                                        self.icount -= 1;
                                        break;
                                    }
                                    self.pc = v as u16;
                                } else {
                                    let ea = self.ea_pre_0(opcode) as u32;
                                    let v = self.read_data_stall(bus, ea);
                                    if self.stall {
                                        self.pc = self.ppc;
                                        self.stall = false;
                                        self.cov.stall_retries += 1;
                                        self.icount -= 1;
                                        break;
                                    }
                                    self.ea_post_0(opcode);
                                    self.pc = v as u16;
                                }
                            }
                            2 => {
                                self.pcs_push();
                                self.pc = data;
                            }
                            3 => {
                                if (opcode & 0x4000) != 0 {
                                    let v = self.read_reg(bus, opcode);
                                    if self.stall {
                                        self.pc = self.ppc;
                                        self.stall = false;
                                        self.cov.stall_retries += 1;
                                        self.icount -= 1;
                                        break;
                                    }
                                    self.pcs_push();
                                    self.pc = v as u16;
                                } else {
                                    let ea = self.ea_pre_0(opcode) as u32;
                                    let v = self.read_data_stall(bus, ea);
                                    if self.stall {
                                        self.pc = self.ppc;
                                        self.stall = false;
                                        self.cov.stall_retries += 1;
                                        self.icount -= 1;
                                        break;
                                    }
                                    self.ea_post_0(opcode);
                                    self.pcs_push();
                                    self.pc = v as u16;
                                }
                            }
                            5 => {
                                self.pcs_pop();
                            }
                            6 => {
                                let ea = self.ea_pre_0(opcode) as u32;
                                let v = self.read_data_stall(bus, ea);
                                if self.stall {
                                    self.pc = self.ppc;
                                    self.stall = false;
                                    self.cov.stall_retries += 1;
                                    self.icount -= 1;
                                    break;
                                }
                                self.ea_post_0(opcode);
                                self.write_reg(bus, opcode >> 9, v);
                            }
                            _ => {}
                        }
                    }

                    if subtype < 2 {
                        match cond {
                            0x10 => {
                                if self.c0 != 1 {
                                    self.c0 = self.c0.wrapping_sub(1);
                                    if self.c0 == 1 {
                                        self.st |= F_ZC0;
                                    }
                                }
                            }
                            0x11 if self.c1 != 1 => {
                                self.c1 = self.c1.wrapping_sub(1);
                                if self.c1 == 1 {
                                    self.st |= F_ZC1;
                                }
                            }
                            _ => {}
                        }
                    }
                }

                _ => {
                    self.cov.group_unknown[group as usize] += 1;
                }
            }

            // Repetition Logic
            // If the current instruction was REP, we skip this block to allow PC to advance.
            if !is_rep && self.r != 1 {
                self.pc = self.ppc;
                self.r = self.r.wrapping_sub(1);
            }

            self.icount -= 1;
            if bus.halt_requested() {
                break;
            }
        }
    }

    fn write_mem_internal_1(&mut self, bus: &mut impl Mb86233Bus, r: u32, v: u32, bank: bool) {
        let mut ea = self.ea_pre_1(r) as u32;
        if bank {
            ea = ea.wrapping_add(0x200);
        }
        bus.write_data(ea, v);
        self.ea_post_1(r);
    }

    fn write_mem_io_1(&mut self, bus: &mut impl Mb86233Bus, r: u32, v: u32) {
        let ea = self.ea_pre_1(r) as u32;
        bus.write_io(ea, v);
        self.ea_post_1(r);
    }
}
