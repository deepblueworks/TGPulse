use crate::bus::Bus;
use crate::cpu::defs::I960Cpu;

impl I960Cpu {
    // --- Internal FPU Helpers ---

    /// Convert u32 bits to f32 then extend to f64
    fn u2f(v: u32) -> f64 {
        f32::from_bits(v) as f64
    }

    /// Convert f64 to f32 then bits to u32
    fn f2u(v: f64) -> u32 {
        (v as f32).to_bits()
    }

    /// Convert u64 bits directly to f64 (double precision)
    fn u2d(v: u64) -> f64 {
        f64::from_bits(v)
    }

    /// Convert f64 directly to u64 bits
    fn d2u(v: f64) -> u64 {
        v.to_bits()
    }

    // --- Operand Fetchers (Real/Floating) ---

    /// Get 1st Real Operand (Register or Literal)
    fn get_1_rif(&self, opcode: u32) -> f64 {
        if (opcode & 0x800) == 0 {
            Self::u2f(self.r[(opcode & 0x1f) as usize])
        } else {
            let idx = opcode & 0x1f;
            if idx < 4 {
                self.fp[idx as usize]
            } else if idx == 0x16 {
                1.0
            } else {
                0.0
            }
        }
    }

    /// Get 2nd Real Operand
    fn get_2_rif(&self, opcode: u32) -> f64 {
        if (opcode & 0x1000) == 0 {
            Self::u2f(self.r[((opcode >> 14) & 0x1f) as usize])
        } else {
            let idx = (opcode >> 14) & 0x1f;
            if idx < 4 {
                self.fp[idx as usize]
            } else if idx == 0x16 {
                1.0
            } else {
                0.0
            }
        }
    }

    /// Set Real Result (32-bit float or FP reg)
    fn set_rif(&mut self, opcode: u32, val: f64) {
        if (opcode & 0x2000) == 0 {
            self.r[((opcode >> 19) & 0x1f) as usize] = Self::f2u(val);
        } else if (opcode & 0x00e00000) == 0 {
            self.fp[((opcode >> 19) & 3) as usize] = val;
        } else {
            // Literal as destination is illegal in hardware
        }
    }

    /// Writes a 64-bit integer result into an aligned register pair.
    fn set_ri64(&mut self, opcode: u32, val: u64) {
        if (opcode & 0x2000) == 0 {
            let idx = ((opcode >> 19) & 0x1f) as usize;
            self.r[idx] = val as u32;
            self.r[idx + 1] = (val >> 32) as u32;
        }
        // A literal destination is illegal in hardware.
    }

    /// Get 1st Long Real Operand (64-bit)
    fn get_1_rifl(&self, opcode: u32) -> f64 {
        if (opcode & 0x800) == 0 {
            let idx = (opcode & 0x1e) as usize; // aligned
            let v = (self.r[idx] as u64) | ((self.r[idx + 1] as u64) << 32);
            Self::u2d(v)
        } else {
            let idx = opcode & 0x1f;
            if idx < 4 {
                self.fp[idx as usize]
            } else if idx == 0x16 {
                1.0
            } else {
                0.0
            }
        }
    }

    /// Get 2nd Long Real Operand (64-bit)
    fn get_2_rifl(&self, opcode: u32) -> f64 {
        if (opcode & 0x1000) == 0 {
            let idx = ((opcode >> 14) & 0x1e) as usize; // aligned
            let v = (self.r[idx] as u64) | ((self.r[idx + 1] as u64) << 32);
            Self::u2d(v)
        } else {
            let idx = (opcode >> 14) & 0x1f;
            if idx < 4 {
                self.fp[idx as usize]
            } else if idx == 0x16 {
                1.0
            } else {
                0.0
            }
        }
    }

    /// Set Long Real Result
    fn set_rifl(&mut self, opcode: u32, val: f64) {
        if (opcode & 0x2000) == 0 {
            let v = Self::d2u(val);
            let idx = ((opcode >> 19) & 0x1e) as usize;
            self.r[idx] = v as u32;
            self.r[idx + 1] = (v >> 32) as u32;
        } else if (opcode & 0x00e00000) == 0 {
            self.fp[((opcode >> 19) & 3) as usize] = val;
        }
    }

    /// Compare Double
    fn cmp_d(&mut self, v1: f64, v2: f64) {
        self.ac &= !7;
        if v1 < v2 {
            self.ac |= 4;
        } else if v1 == v2 {
            self.ac |= 2;
        } else if v1 > v2 {
            self.ac |= 1;
        }
    }

    /// Helper for Rounding Instructions
    fn round_to_int(&self, val: f64) -> f64 {
        // AC bits 30-31 determine rounding mode
        // 00: Nearest (Round to even if halfway, or away from zero? Rust/C++ 'round' is away from zero)
        // 01: Down (Floor)
        // 10: Up (Ceil)
        // 11: Truncate (Toward Zero)
        match (self.ac >> 30) & 3 {
            0 => val.round(),
            1 => val.floor(),
            2 => val.ceil(),
            _ => val.trunc(),
        }
    }

    /// Whether `op_fpu` has a real implementation for this (major, sub) pair.
    fn fpu_handled(op_idx: u32, sub: u32) -> bool {
        match op_idx {
            0x67 => matches!(sub, 0x0 | 0x1 | 0x4 | 0x5 | 0x6 | 0x7),
            0x68 => matches!(sub, 0x0..=0x3 | 0x5 | 0x8..=0xe),
            0x69 => matches!(sub, 0x0 | 0x2 | 0x5 | 0x8..=0xe),
            0x6c => matches!(sub, 0x0 | 0x1 | 0x2 | 0x3 | 0x9),
            0x6d => sub == 0x9,
            0x6e => matches!(sub, 0x1 | 0x2),
            0x78 | 0x79 => matches!(sub, 0xb | 0xc | 0xd | 0xf),
            _ => false,
        }
    }

    // --- MAIN FPU DISPATCHER ---

    pub fn op_fpu<B: Bus>(&mut self, _bus: &mut B, opcode: u32) {
        let op_idx = opcode >> 24;
        let sub = (opcode >> 7) & 0xf;

        // A real part faults on an unhandled FP sub-op; we would silently leave
        // the destination stale, which is far harder to spot -- Sega Rally's
        // gamma table came out saturated for exactly that reason. Record the
        // distinct pairs so a new game's gaps show up immediately.
        if !Self::fpu_handled(op_idx, sub) && !self.fpu_unimpl.contains(&(op_idx, sub)) {
            self.fpu_unimpl.push((op_idx, sub));
        }

        match op_idx {
            // 0x67: Conversion / Scale
            0x67 => match sub {
                0x0 => {
                    // emul
                    self.icount -= 37;
                    let t1 = self.get_1_ri(opcode) as u64;
                    let t2 = self.get_2_ri(opcode) as u64;
                    let res = t1 * t2;
                    if (opcode & 0x2000) == 0 {
                        let idx = ((opcode >> 19) & 0x1f) as usize;
                        self.r[idx] = res as u32;
                        self.r[idx + 1] = (res >> 32) as u32;
                    }
                }
                0x1 => {
                    // ediv
                    self.icount -= 37;
                    let src1 = self.get_1_ri(opcode) as u64;
                    let idx2 = ((opcode >> 14) & 0x1f) as usize;
                    let src2 = if (opcode & 0x1000) == 0 {
                        (self.r[idx2] as u64) | ((self.r[idx2 + 1] as u64) << 32)
                    } else {
                        idx2 as u64
                    };

                    if src1 != 0 {
                        let rem = src2 % src1;
                        let quot = src2 / src1;
                        if (opcode & 0x2000) == 0 {
                            let dst_idx = ((opcode >> 19) & 0x1f) as usize;
                            self.r[dst_idx] = rem as u32;
                            self.r[dst_idx + 1] = quot as u32;
                        }
                    }
                }
                0x4 => {
                    // cvtir
                    self.icount -= 30;
                    let t1 = self.get_1_ri(opcode) as i32;
                    self.set_rif(opcode, t1 as f64);
                }
                0x5 => {
                    // cvtilr
                    self.icount -= 30;
                    let t1 = self.get_1_ri(opcode) as i32;
                    self.set_rifl(opcode, t1 as f64);
                }
                0x6 => {
                    // scalerl
                    self.icount -= 30;
                    let t1 = self.get_1_ri(opcode) as i32;
                    let t2f = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2f * 2.0f64.powi(t1));
                }
                0x7 => {
                    // scaler
                    self.icount -= 30;
                    let t1 = self.get_1_ri(opcode) as i32;
                    let t2f = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2f * 2.0f64.powi(t1));
                }
                _ => {}
            },

            // 0x68: Real Arithmetic
            0x68 => match sub {
                0x0 => {
                    self.icount -= 267;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2.atan2(t1));
                } // atanr
                0x1 => {
                    self.icount -= 400;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 * (t1 + 1.0).log2());
                } // logepr
                0x2 => {
                    self.icount -= 438;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 * t1.log2());
                } // logr
                0x3 => {
                    self.icount -= 67;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 % t1);
                } // remr
                0x5 => {
                    self.icount -= 10;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.cmp_d(t1, t2);
                } // cmpr
                0x8 => {
                    self.icount -= 104;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1.sqrt());
                } // sqrtr
                0x9 => {
                    self.icount -= 334;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, 2.0f64.powf(t1) - 1.0);
                } // expr
                0xa => {
                    self.icount -= 37;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1.abs().log2().floor());
                } // logbnr
                0xb => {
                    self.icount -= 69;
                    let t1 = self.get_1_rif(opcode);
                    let v = self.round_to_int(t1);
                    self.set_rif(opcode, v);
                } // roundr
                0xc => {
                    self.icount -= 406;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1.sin());
                } // sinr
                0xd => {
                    self.icount -= 406;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1.cos());
                } // cosr
                0xe => {
                    self.icount -= 293;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1.tan());
                } // tanr
                _ => {}
            },

            // 0x69: Long Real Arithmetic
            0x69 => match sub {
                0x0 => {
                    self.icount -= 350;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2.atan2(t1));
                } // atanrl
                0x2 => {
                    self.icount -= 438;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2 * t1.log2());
                } // logrl
                0x5 => {
                    self.icount -= 12;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.cmp_d(t1, t2);
                } // cmprl
                0x8 => {
                    self.icount -= 104;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1.sqrt());
                } // sqrtrl
                0x9 => {
                    self.icount -= 334;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, 2.0f64.powf(t1) - 1.0);
                } // exprl
                0xa => {
                    self.icount -= 37;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1.abs().log2().floor());
                } // logbnrl
                0xb => {
                    self.icount -= 70;
                    let t1 = self.get_1_rifl(opcode);
                    let v = self.round_to_int(t1);
                    self.set_rifl(opcode, v);
                } // roundrl
                0xc => {
                    self.icount -= 441;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1.sin());
                } // sinrl
                0xd => {
                    self.icount -= 441;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1.cos());
                } // cosrl
                0xe => {
                    self.icount -= 323;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1.tan());
                } // tanrl
                _ => {}
            },

            // 0x6C: Convert Real to Int
            0x6c => match sub {
                0x0 => {
                    self.icount -= 33;
                    let t1 = self.get_1_rif(opcode);
                    self.set_ri(opcode, self.round_to_int(t1) as i32 as u32);
                } // cvtri
                0x1 => {
                    self.icount -= 35;
                    let t1 = self.get_1_rif(opcode);
                    self.set_ri64(opcode, self.round_to_int(t1) as i64 as u64);
                } // cvtril
                0x2 => {
                    self.icount -= 43;
                    let t1 = self.get_1_rif(opcode);
                    self.set_ri(opcode, t1 as i32 as u32);
                } // cvtzri
                0x3 => {
                    self.icount -= 44;
                    let t1 = self.get_1_rif(opcode);
                    self.set_ri64(opcode, t1 as i64 as u64);
                } // cvtzril
                0x9 => {
                    self.icount -= 5;
                    let t1 = self.get_1_rif(opcode);
                    self.set_rif(opcode, t1);
                } // movr
                _ => {}
            },

            // 0x6D: Mov Long Real
            0x6d => {
                if sub == 0x9 {
                    self.icount -= 6;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1);
                }
            }

            // 0x6E: Move Extended / Copy
            0x6e => match sub {
                0x1 => {
                    // movre
                    self.icount -= 8;
                    let t1 = self.get_1_rifl(opcode);
                    self.set_rifl(opcode, t1);
                }
                0x2 => {
                    // cpysre
                    self.icount -= 8;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, if t2 >= 0.0 { t1.abs() } else { -t1.abs() });
                }
                _ => {}
            },

            // 0x78: Real Ops
            0x78 => match sub {
                0xb => {
                    self.icount -= 35;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 / t1);
                } // divr
                0xc => {
                    self.icount -= 18;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 * t1);
                } // mulr
                0xd => {
                    self.icount -= 10;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 - t1);
                } // subr
                0xf => {
                    self.icount -= 10;
                    let t1 = self.get_1_rif(opcode);
                    let t2 = self.get_2_rif(opcode);
                    self.set_rif(opcode, t2 + t1);
                } // addr
                _ => {}
            },

            // 0x79: Long Real Ops
            0x79 => match sub {
                0xb => {
                    self.icount -= 77;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2 / t1);
                } // divrl
                0xc => {
                    self.icount -= 36;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2 * t1);
                } // mulrl
                0xd => {
                    self.icount -= 13;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2 - t1);
                } // subrl
                0xf => {
                    self.icount -= 13;
                    let t1 = self.get_1_rifl(opcode);
                    let t2 = self.get_2_rifl(opcode);
                    self.set_rifl(opcode, t2 + t1);
                } // addrl
                _ => {}
            },

            _ => {}
        }
    }
}
