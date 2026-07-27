//! The ALU and multiplier slots and
//! `decode_mulop`. Both write into the shared MA/MB/AA/AB register banks.

use crate::state::flag::*;
use crate::state::Mb86235;

#[inline]
fn u2f(v: u32) -> f32 {
    f32::from_bits(v)
}
#[inline]
fn f2u(v: f32) -> u32 {
    v.to_bits()
}

impl Mb86235 {
    #[inline]
    fn fclr(&mut self, m: u32) {
        self.st &= !m;
    }
    #[inline]
    fn fset(&mut self, m: u32) {
        self.st |= m;
    }

    fn set_flags_d(&mut self, val: u32) {
        self.fclr(AN | AZ);
        if val & 0x8000_0000 != 0 {
            self.fset(AN);
        }
        if val == 0 {
            self.fset(AZ);
        }
    }
    fn set_flags_f(&mut self, val: f32) {
        self.fclr(AN | AZ);
        if val < 0.0 {
            self.fset(AN);
        }
        if val == 0.0 {
            self.fset(AZ);
        }
    }
    fn set_flags_i(&mut self, val: i32) {
        self.fclr(AN | AZ);
        if val < 0 {
            self.fset(AN);
        }
        if val == 0 {
            self.fset(AZ);
        }
    }

    /// PR ring read, with the post-action the low bits select.
    fn get_prx(&mut self, which: u8) -> u32 {
        let res = self.pr[self.prp as usize];
        match which & 7 {
            1 => self.prp = (self.prp + 1) % 24,
            2 => self.prp = if self.prp == 0 { 23 } else { self.prp - 1 },
            3 => self.prp = 0,
            _ => {}
        }
        res
    }

    fn const_float(which: u8) -> u32 {
        const T: [f32; 8] = [-1.0, 0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 5.0];
        f2u(T[(which & 7) as usize])
    }
    fn const_int(which: u8) -> u32 {
        match which & 7 {
            0 => 0,
            1 => 1,
            _ => 0xffff_ffff,
        }
    }

    fn get_alureg(&mut self, which: u8, isfloat: bool) -> u32 {
        let n = (which & 7) as usize;
        match which >> 3 {
            0 => self.aa[n],
            1 => self.ab[n],
            2 => self.get_prx(which & 7),
            _ => {
                if isfloat {
                    Self::const_float(which & 7)
                } else {
                    Self::const_int(which & 7)
                }
            }
        }
    }

    fn get_mulreg(&mut self, which: u8, isfloat: bool) -> u32 {
        let n = (which & 7) as usize;
        match which >> 3 {
            0 => self.ma[n],
            1 => self.mb[n],
            2 => self.get_prx(which & 7),
            _ => {
                if isfloat {
                    Self::const_float(which & 7)
                } else {
                    Self::const_int(which & 7)
                }
            }
        }
    }

    fn set_alureg(&mut self, which: u8, value: u32) {
        let n = (which & 7) as usize;
        match which >> 3 {
            0 => self.ma[n] = value,
            1 => self.mb[n] = value,
            2 => self.aa[n] = value,
            _ => self.ab[n] = value,
        }
    }

    /// Whether an ALU opcode reads a second source. The logical, ATRx, ABS
    /// and single-operand floating-point forms do not.
    pub(crate) fn alu_has_second_src(which: u8) -> bool {
        if which & 0x1c == 0x1c {
            return false; // logical
        }
        if which & 0x1e == 0x16 {
            return false; // ATR/ATRZ
        }
        if which & 0x0f == 0x05 {
            return false; // ABS/FABS
        }
        if which & 0x18 == 0x08 {
            return false; // single-operand float
        }
        true
    }

    fn decode_aluop(&mut self, opcode: u8, src1: u32, src2: u32, imm: u8, dst: u8) {
        match opcode {
            // --- floating point ---
            0x00..=0x03 => {
                // FADD / FADDZ / FSUB / FSUBZ
                let (f1, f2) = (u2f(src1), u2f(src2));
                let mut d = if opcode & 2 != 0 { f2 - f1 } else { f1 + f2 };
                if opcode & 1 != 0 {
                    self.fclr(ZC);
                    if d < 0.0 {
                        self.fset(ZC);
                        d = 0.0;
                    }
                }
                self.set_flags_f(d);
                self.set_alureg(dst, f2u(d));
            }
            0x04 | 0x06 => {
                // FCMP / FABC
                let (f1, f2) = (u2f(src1), u2f(src2));
                let d = if opcode & 2 != 0 {
                    f2.abs() - f1.abs()
                } else {
                    f2 - f1
                };
                self.set_flags_f(d);
            }
            0x05 => {
                let d = u2f(src1).abs();
                self.set_flags_f(d);
                self.set_alureg(dst, f2u(d));
            }
            0x07 => {} // NOP
            0x08 | 0x09 => {
                // FEA / FES: bias the exponent
                let mut exp = (src1 >> 23) & 0xff;
                let mut v = src1 & 0x7f80_0000;
                if opcode & 1 != 0 {
                    exp = exp.wrapping_sub(imm as u32);
                } else {
                    exp = exp.wrapping_add(imm as u32);
                }
                exp &= 0xff;
                v |= exp << 23;
                self.set_flags_d(v);
                self.set_alureg(dst, v);
            }
            0x0a => {
                // FRCP
                let f = u2f(src1);
                self.fclr(ZD);
                if f == 0.0 {
                    self.fset(ZD);
                }
                let r = 1.0 / f;
                self.set_flags_f(r);
                self.set_alureg(dst, f2u(r));
            }
            0x0b => {
                // FRSQ
                let f = u2f(src1);
                self.fclr(NR);
                if f <= 0.0 {
                    self.fset(NR);
                }
                let r = 1.0 / f.sqrt();
                self.set_flags_f(r);
                self.set_alureg(dst, f2u(r));
            }
            0x0c => {
                // FLOG. The reference computes `log(f) / 0.301030f` and calls it log2,
                // though dividing a natural log by log10(2) is neither log2 nor
                // log10. The constant and the natural log are reproduced as they
                // are: matching the reference is what makes a divergence here
                // attributable, and no game has been seen to depend on it.
                #[allow(clippy::approx_constant)]
                let scale = 0.301_030_f32;
                let f = u2f(src1);
                self.fclr(IL);
                if f <= 0.0 {
                    self.fset(IL);
                }
                let r = f.ln() / scale;
                self.set_flags_f(r);
                self.set_alureg(dst, f2u(r));
            }
            0x0d => {
                // CIF: int -> float
                let f = (src1 as i32) as f32;
                self.set_flags_f(f);
                self.set_alureg(dst, f2u(f));
            }
            0x0e => {
                // CFI: float -> int
                let v = u2f(src1) as i32;
                self.set_flags_i(v);
                self.set_alureg(dst, v as u32);
            }
            0x0f => {
                // CFIB: float -> byte, saturating
                let f = u2f(src1);
                let mut res = f as u32;
                if f < 0.0 {
                    self.fset(AU);
                    res = 0;
                }
                self.fclr(AZ);
                if res == 0 {
                    self.fset(AZ);
                }
                if res > 0xff {
                    self.fset(AV);
                    res = 0xff;
                }
                self.set_alureg(dst, res);
            }

            // --- integer ---
            0x10..=0x13 => {
                // ADD / ADDZ / SUB / SUBZ
                let (v1, v2) = (src1 as i32, src2 as i32);
                let mut res = if opcode & 2 != 0 {
                    v2.wrapping_sub(v1)
                } else {
                    v1.wrapping_add(v2)
                };
                if opcode & 1 != 0 {
                    self.fclr(ZC);
                    if res < 0 {
                        self.fset(ZC);
                        res = 0;
                    }
                }
                self.set_flags_i(res);
                self.set_alureg(dst, res as u32);
            }
            0x14 => {
                // CMP
                let res = (src2 as i32).wrapping_sub(src1 as i32);
                self.set_flags_i(res);
            }
            0x15 => {
                // ABS
                let v = src1 & 0x7fff_ffff;
                self.set_flags_d(v);
                self.set_alureg(dst, v);
            }
            0x16 | 0x17 => {
                // ATR / ATRZ
                let mut v = src1;
                if opcode & 1 != 0 {
                    self.fclr(ZC);
                    if v & 0x8000_0000 != 0 {
                        self.fset(ZC);
                        v = 0;
                    }
                }
                self.set_alureg(dst, v);
            }

            // --- logical / shifts ---
            0x18 => {
                let r = src1 & src2;
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x19 => {
                let r = src1 | src2;
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x1a => {
                let r = src1 ^ src2;
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x1b => {
                let r = !src1;
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x1c => {
                let r = src1 >> (imm & 31);
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x1d => {
                let r = src1 << (imm & 31);
                self.set_flags_d(r);
                self.set_alureg(dst, r);
            }
            0x1e => {
                let r = (src1 as i32) >> (imm & 31);
                self.set_flags_i(r);
                self.set_alureg(dst, r as u32);
            }
            0x1f => {
                let r = (src1 as i32) << (imm & 31);
                self.set_flags_i(r);
                self.set_alureg(dst, r as u32);
            }
            _ => {}
        }
    }

    fn decode_mulop(&mut self, isfmul: bool, src1: u32, src2: u32, dst: u8) {
        if isfmul {
            let res = u2f(src1) * u2f(src2);
            // MV and MU are sticky here; the reference does not clear them.
            self.fclr(MN | MZ | MD);
            if res < 0.0 {
                self.fset(MN);
            }
            if res == 0.0 {
                self.fset(MZ);
            }
            if res.is_infinite() {
                self.fset(MV);
            }
            if res.abs() < f32::MIN_POSITIVE {
                self.fset(MU);
            }
            if res.is_nan() {
                self.fset(MD);
            }
            self.set_alureg(dst, f2u(res));
        } else {
            let res = (src1 as i32).wrapping_mul(src2 as i32);
            self.fclr(MN | MZ);
            if res < 0 {
                self.fset(MN);
            }
            if res == 0 {
                self.fset(MZ);
            }
            self.set_alureg(dst, res as u32);
        }
    }

    // --- opcode field accessors ---
    #[inline]
    fn aop(op: u64) -> u8 {
        ((op >> 56) & 0x1f) as u8
    }
    #[inline]
    fn ai1(op: u64) -> u8 {
        ((op >> 52) & 0x0f) as u8
    }
    #[inline]
    fn ai2(op: u64) -> u8 {
        ((op >> 47) & 0x1f) as u8
    }
    #[inline]
    fn ao(op: u64) -> u8 {
        ((op >> 42) & 0x1f) as u8
    }
    #[inline]
    fn mop(op: u64) -> u8 {
        ((op >> 41) & 0x01) as u8
    }
    #[inline]
    fn mi1(op: u64) -> u8 {
        ((op >> 37) & 0x0f) as u8
    }
    #[inline]
    fn mi2(op: u64) -> u8 {
        ((op >> 32) & 0x1f) as u8
    }
    #[inline]
    fn mo(op: u64) -> u8 {
        ((op >> 27) & 0x1f) as u8
    }

    /// Single-slot form: bit 41 selects ALU or multiplier.
    pub(crate) fn do_alu1_op(&mut self, op: u64) {
        if self.stalled {
            return;
        }
        if op & (1 << 41) != 0 {
            let aluop = Self::aop(op);
            let src1 = self.get_alureg(Self::ai1(op), false);
            let src2 = if Self::alu_has_second_src(aluop) {
                self.get_alureg(Self::ai2(op), aluop & 0x10 == 0)
            } else {
                0
            };
            self.decode_aluop(aluop, src1, src2, Self::ai2(op), Self::ao(op));
        } else {
            let isfmul = Self::aop(op) != 0;
            let src1 = self.get_mulreg(Self::ai1(op), false);
            let src2 = self.get_mulreg(Self::ai2(op), isfmul);
            self.decode_mulop(isfmul, src1, src2, Self::ao(op));
        }
    }

    /// Dual-slot form: an ALU operation and a multiply issue together.
    pub(crate) fn do_alu2_op(&mut self, op: u64) {
        if self.stalled {
            return;
        }
        let aluop = Self::aop(op);
        let alusrc1 = self.get_alureg(Self::ai1(op), false);
        let alusrc2 = if Self::alu_has_second_src(aluop) {
            self.get_alureg(Self::ai2(op), aluop & 0x10 == 0)
        } else {
            0
        };
        let isfmul = Self::mop(op) != 0;
        let mulsrc1 = self.get_mulreg(Self::mi1(op), false);
        let mulsrc2 = self.get_mulreg(Self::mi2(op), isfmul);

        self.decode_aluop(aluop, alusrc1, alusrc2, Self::ai2(op), Self::ao(op));
        self.decode_mulop(isfmul, mulsrc1, mulsrc2, Self::mo(op));
    }
}
