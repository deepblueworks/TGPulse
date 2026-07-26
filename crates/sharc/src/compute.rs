//! Compute units: fixed-point ALU, floating-point ALU, and multiplier, plus
//! the `COMPUTE` dispatcher. Flag semantics are kept exactly; the register
//! file is `self.r[..]`, read as either integer or float.

use crate::consts::*;
use crate::state::Sharc;
use crate::tables::{RECIPS_MANTISSA_LOOKUP, RSQRTS_MANTISSA_LOOKUP};

const FLOAT_SIGN_MASK: u32 = 0x8000_0000;
const FLOAT_EXPONENT_MASK: u32 = 0x7f80_0000;
const FLOAT_MANTISSA_MASK: u32 = 0x007f_ffff;
const FLOAT_INFINITY: u32 = 0x7f80_0000;
const FLOAT_CANONICAL_NAN: u32 = 0xffff_ffff;
const FLOAT_EXPONENT_SHIFT: u32 = 23;
const FLOAT_EXPONENT_BIAS: i32 = 127;

#[inline]
fn is_float_zero(r: u32) -> bool {
    r & (FLOAT_EXPONENT_MASK | FLOAT_MANTISSA_MASK) == 0
}
#[inline]
fn is_float_denormal(r: u32) -> bool {
    (r & FLOAT_EXPONENT_MASK == 0) && (r & FLOAT_MANTISSA_MASK != 0)
}
#[inline]
fn is_float_denormal_or_zero(r: u32) -> bool {
    r & FLOAT_EXPONENT_MASK == 0
}
#[inline]
fn is_float_nan(r: u32) -> bool {
    (r & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK) && (r & FLOAT_MANTISSA_MASK != 0)
}
#[inline]
fn is_float_infinity(r: u32) -> bool {
    r & (FLOAT_EXPONENT_MASK | FLOAT_MANTISSA_MASK) == FLOAT_INFINITY
}
fn is_float_nan_add(a: u32, b: u32) -> bool {
    let ae = a & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    let be = b & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    (ae && (a & FLOAT_MANTISSA_MASK != 0))
        || (be && (b & FLOAT_MANTISSA_MASK != 0))
        || (ae && be && ((a ^ b) & FLOAT_SIGN_MASK != 0))
}
fn is_float_nan_sub(a: u32, b: u32) -> bool {
    let ae = a & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    let be = b & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    (ae && (a & FLOAT_MANTISSA_MASK != 0))
        || (be && (b & FLOAT_MANTISSA_MASK != 0))
        || (ae && be && ((a ^ b) & FLOAT_SIGN_MASK == 0))
}
fn is_float_nan_mul(a: u32, b: u32) -> bool {
    let ae = a & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    let be = b & FLOAT_EXPONENT_MASK == FLOAT_EXPONENT_MASK;
    (ae && ((a & FLOAT_MANTISSA_MASK != 0) || is_float_zero(b)))
        || (be && ((b & FLOAT_MANTISSA_MASK != 0) || is_float_zero(a)))
}
#[inline]
fn flush_denormal(r: u32) -> u32 {
    if is_float_denormal_or_zero(r) {
        r & FLOAT_SIGN_MASK
    } else {
        r
    }
}
#[inline]
fn get_unbiased_exponent(f: u32) -> i32 {
    (((f >> FLOAT_EXPONENT_SHIFT) & 0xff) as i32) - FLOAT_EXPONENT_BIAS
}
#[inline]
fn make_biased_exponent(e: i32) -> u32 {
    (((e + FLOAT_EXPONENT_BIAS) & 0xff) as u32) << FLOAT_EXPONENT_SHIFT
}

impl Sharc {
    // --- flag helpers ---
    #[inline]
    fn clear_alu_flags(&mut self) {
        self.astat &= !(AZ | AN | AV | AC | AS | AI);
    }
    #[inline]
    fn clear_mul_flags(&mut self) {
        self.astat &= !(MN | MV | MU | MI);
    }
    #[inline]
    fn flag_az(&mut self, r: u32) {
        if r == 0 {
            self.astat |= AZ;
        }
    }
    #[inline]
    fn flag_an(&mut self, r: u32) {
        if r & 0x8000_0000 != 0 {
            self.astat |= AN;
        }
    }
    #[inline]
    fn flag_ac_add(&mut self, r: u32, a: u32) {
        if r < a {
            self.astat |= AC;
        }
    }
    #[inline]
    fn flag_ac_sub(&mut self, r: u32, a: u32) {
        if r <= a {
            self.astat |= AC;
        }
    }
    #[inline]
    fn flag_av_add(&mut self, r: u32, a: u32, b: u32) {
        if !(a ^ b) & (a ^ r) & 0x8000_0000 != 0 {
            self.astat |= AV;
            self.stky |= AOS;
        }
    }
    #[inline]
    fn flag_av_sub(&mut self, r: u32, a: u32, b: u32) {
        if (a ^ b) & (a ^ r) & 0x8000_0000 != 0 {
            self.astat |= AV;
            self.stky |= AOS;
        }
    }
    #[inline]
    fn saturate(&self, r: u32) -> u32 {
        (((r as i32) >> 31) as u32) ^ 0x8000_0000
    }
    #[inline]
    fn alusat(&self) -> bool {
        self.mode1 & MODE1_ALUSAT != 0
    }

    // --- float ALU primitives (return the raw bits) ---
    fn fadd_bits(&mut self, fx: usize, fy: usize) -> u32 {
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan_add(a, b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let x = f32::from_bits(flush_denormal(a));
            let y = f32::from_bits(flush_denormal(b));
            let mut rr = (x + y).to_bits();
            if is_float_infinity(rr) {
                self.astat |= AV;
                self.stky |= AVS;
                if self.mode1 & MODE1_TRUNCATE != 0 {
                    rr = (rr & FLOAT_SIGN_MASK) | 0x7f7f_ffff;
                }
            } else if is_float_denormal_or_zero(rr) {
                self.astat |= AZ;
                if rr & FLOAT_MANTISSA_MASK != 0 {
                    self.stky |= AUS;
                }
                rr &= FLOAT_SIGN_MASK;
            }
            if rr & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
            r = rr;
        }
        self.astat |= AF;
        r
    }
    fn fsub_bits(&mut self, fx: usize, fy: usize) -> u32 {
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan_sub(a, b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let x = f32::from_bits(flush_denormal(a));
            let y = f32::from_bits(flush_denormal(b));
            let mut rr = (x - y).to_bits();
            if is_float_infinity(rr) {
                self.astat |= AV;
                self.stky |= AVS;
                if self.mode1 & MODE1_TRUNCATE != 0 {
                    rr = (rr & FLOAT_SIGN_MASK) | 0x7f7f_ffff;
                }
            } else if is_float_denormal_or_zero(rr) {
                self.astat |= AZ;
                if rr & FLOAT_MANTISSA_MASK != 0 {
                    self.stky |= AUS;
                }
                rr &= FLOAT_SIGN_MASK;
            }
            if rr & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
            r = rr;
        }
        self.astat |= AF;
        r
    }
    fn fmul_bits(&mut self, fx: usize, fy: usize) -> u32 {
        let (a, b) = (self.r[fx], self.r[fy]);
        if is_float_nan_mul(a, b) {
            self.astat |= MI;
            self.stky |= MIS;
            FLOAT_CANONICAL_NAN
        } else {
            let rr = (f32::from_bits(a) * f32::from_bits(b)).to_bits();
            if rr & FLOAT_SIGN_MASK != 0 {
                self.astat |= MN;
            }
            if is_float_infinity(rr) {
                self.astat |= MV;
                self.stky |= MVS;
            }
            if is_float_denormal(rr) {
                self.astat |= MU;
                self.stky |= MUS;
            }
            rr
        }
    }

    // --- fixed-point ALU ops ---
    pub fn compute_add(&mut self, rn: usize, rx: usize, ry: usize) {
        let (a, b) = (self.r[rx], self.r[ry]);
        let mut r = a.wrapping_add(b);
        self.clear_alu_flags();
        self.flag_av_add(r, a, b);
        self.flag_ac_add(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_sub(&mut self, rn: usize, rx: usize, ry: usize) {
        let (a, b) = (self.r[rx], self.r[ry]);
        let mut r = a.wrapping_sub(b);
        self.clear_alu_flags();
        self.flag_av_sub(r, a, b);
        self.flag_ac_sub(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_add_ci(&mut self, rn: usize, rx: usize, ry: usize) {
        let (a, b) = (self.r[rx], self.r[ry]);
        let c = if self.astat & AC != 0 { 1u32 } else { 0 };
        let mut r = a.wrapping_add(b).wrapping_add(c);
        self.clear_alu_flags();
        self.flag_av_add(r, a, b);
        self.flag_ac_add(r, a);
        if c == 1 && b == 0xffff_ffff {
            self.astat |= AC;
        }
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_sub_ci(&mut self, rn: usize, rx: usize, ry: usize) {
        let (a, b) = (self.r[rx], self.r[ry]);
        let c = if self.astat & AC != 0 { 1u32 } else { 0 };
        let mut r = a.wrapping_sub(b).wrapping_add(c).wrapping_sub(1);
        self.clear_alu_flags();
        self.flag_av_sub(r, a, b);
        if c != 0 || b != 0xffff_ffff {
            self.flag_ac_sub(r, a);
        }
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_add_ci1(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let c = if self.astat & AC != 0 { 1u32 } else { 0 };
        let mut r = a.wrapping_add(c);
        self.clear_alu_flags();
        self.flag_av_add(r, a, 0);
        self.flag_ac_add(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_sub_ci1(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let c = if self.astat & AC != 0 { 1u32 } else { 0 };
        let mut r = a.wrapping_add(c).wrapping_sub(1);
        self.clear_alu_flags();
        self.flag_av_sub(r, a, 0);
        self.flag_ac_sub(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_comp(&mut self, rx: usize, ry: usize) {
        self.clear_alu_flags();
        if self.r[rx] == self.r[ry] {
            self.astat |= AZ;
        } else if (self.r[rx] as i32) < (self.r[ry] as i32) {
            self.astat |= AN;
        }
        let mut comp_accum = (self.astat >> 1) & 0x7f00_0000;
        if self.astat & (AZ | AN) == 0 {
            comp_accum |= 0x8000_0000;
        }
        self.astat &= 0x00ff_ffff;
        self.astat |= comp_accum;
        self.astat &= !AF;
    }
    pub fn compute_inc(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let mut r = a.wrapping_add(1);
        self.clear_alu_flags();
        self.flag_av_add(r, a, 1);
        self.flag_ac_add(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_dec(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let mut r = a.wrapping_sub(1);
        self.clear_alu_flags();
        self.flag_av_sub(r, a, 1);
        self.flag_ac_sub(r, a);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_neg(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let mut r = (a as i32).wrapping_neg() as u32;
        self.clear_alu_flags();
        self.flag_av_sub(r, 0, a);
        self.flag_ac_sub(r, 0);
        if self.alusat() && self.astat & AV != 0 {
            r = self.saturate(r);
        }
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_abs(&mut self, rn: usize, rx: usize) {
        let a = self.r[rx];
        let mut r = (a as i32).unsigned_abs();
        self.clear_alu_flags();
        if (r as i32) < 0 {
            self.astat |= AV;
            self.stky |= AOS;
            if self.mode1 & MODE1_ALUSAT != 0 {
                r = 0x7fff_ffff;
            }
        }
        self.flag_an(r);
        self.flag_az(r);
        if (a as i32) < 0 {
            self.astat |= AS;
        }
        self.r[rn] = r;
        self.astat &= !AF;
    }
    pub fn compute_pass(&mut self, rn: usize, rx: usize) {
        self.clear_alu_flags();
        let r = self.r[rx];
        self.r[rn] = r;
        if r == 0 {
            self.astat |= AZ;
        }
        if r & 0x8000_0000 != 0 {
            self.astat |= AN;
        }
        self.astat &= !AF;
    }
    fn logic(&mut self, rn: usize, r: u32) {
        self.clear_alu_flags();
        self.flag_an(r);
        self.flag_az(r);
        self.r[rn] = r;
        self.astat &= !AF;
    }
    // --- Shifter (compute unit 2) ---------------------------------------
    //
    //hxx`. Every shifter operation clears
    // SZ/SV/SS first, then sets SZ on a zero result and SV when the shift or
    // bit field ran past the width of the word. Sonic Championship drives its
    // hit detection through these, so leaving them out makes every collision
    // test read back as "no contact".
    #[inline]
    fn shift_begin(&mut self) {
        self.astat &= !(crate::consts::SZ | crate::consts::SV | crate::consts::SS);
    }

    #[inline]
    fn shift_sz(&mut self, v: u32) {
        if v == 0 {
            self.astat |= crate::consts::SZ;
        }
    }

    #[inline]
    fn shift_sv(&mut self) {
        self.astat |= crate::consts::SV;
    }

    /// LSHIFT Rx BY Ry
    pub fn compute_lshift(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry] as i32;
        let v = if shift < 0 {
            if shift > -32 {
                self.r[rx] >> -shift
            } else {
                0
            }
        } else {
            let v = if shift < 32 { self.r[rx] << shift } else { 0 };
            if shift > 0 {
                self.shift_sv();
            }
            v
        };
        self.r[rn] = v;
        self.shift_sz(v);
    }

    /// ROT Rx BY Ry
    pub fn compute_rot(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry] as i32;
        let v = if shift < 0 {
            self.r[rx].rotate_right((-shift) as u32 & 31)
        } else {
            let v = self.r[rx].rotate_left(shift as u32 & 31);
            if shift > 0 {
                self.shift_sv();
            }
            v
        };
        self.r[rn] = v;
        self.shift_sz(v);
    }

    /// Rn = Rn OR LSHIFT Rx BY Ry. The shift count is a signed byte here,
    /// not a full word.
    pub fn compute_or_lshift(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry] as u8 as i8 as i32;
        let v = if shift < 0 {
            if shift > -32 {
                self.r[rx] >> -shift
            } else {
                0
            }
        } else {
            let v = if shift < 32 { self.r[rx] << shift } else { 0 };
            if shift > 0 {
                self.shift_sv();
            }
            v
        };
        let v = self.r[rn] | v;
        self.r[rn] = v;
        self.shift_sz(v);
    }

    /// FEXT Rx BY Ry, optionally sign-extending the extracted field.
    pub fn compute_fext(&mut self, rn: usize, rx: usize, ry: usize, signed: bool) {
        self.shift_begin();
        let bit = self.r[ry] & 0x3f;
        let len = (self.r[ry] >> 6) & 0x3f;
        let v = if len == 0 || bit >= 32 {
            0
        } else if signed && bit + len > 32 {
            self.r[rx] >> bit
        } else {
            let n = len.min(32);
            let field = (self.r[rx] >> bit) & mask_bits(n);
            if signed {
                sign_extend(field, n)
            } else {
                field
            }
        };
        self.r[rn] = v;
        self.shift_sz(v);
        if bit + len > 32 {
            self.shift_sv();
        }
    }

    /// Rn = Rn OR FDEP Rx BY Ry
    pub fn compute_or_fdep(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let bit = self.r[ry] & 0x3f;
        let len = (self.r[ry] >> 6) & 0x3f;
        if len != 0 && bit < 32 {
            let field = self.r[rx] & mask_bits(len.min(32));
            self.r[rn] |= field << bit;
        }
        let v = self.r[rn];
        self.shift_sz(v);
        if bit + len > 32 {
            self.shift_sv();
        }
    }

    /// BSET / BCLR Rx BY Ry
    pub fn compute_bit_set(&mut self, rn: usize, rx: usize, ry: usize, set: bool) {
        self.shift_begin();
        let shift = self.r[ry];
        let mut v = self.r[rx];
        if shift < 32 {
            if set {
                v |= 1 << shift;
            } else {
                v &= !(1 << shift);
            }
        } else {
            self.shift_sv();
        }
        self.r[rn] = v;
        self.shift_sz(v);
    }

    /// BTST Rx BY Ry -- reports through SZ rather than writing a register.
    pub fn compute_btst(&mut self, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry];
        if shift < 32 {
            let r = self.r[rx] & (1 << shift);
            self.shift_sz(r);
        } else {
            self.astat |= crate::consts::SZ | crate::consts::SV;
        }
    }

    /// SCALB's raw form: scales `a` by 2^ry without touching the flags, so the
    /// scaled variants below can reuse it.
    fn scalb_bits(&self, a: u32, ry: usize) -> u32 {
        if is_float_nan(a) {
            return FLOAT_CANONICAL_NAN;
        }
        let mantissa = a & FLOAT_MANTISSA_MASK;
        let sign = a & FLOAT_SIGN_MASK;
        let exponent = get_unbiased_exponent(a) + self.r[ry] as i32;
        if exponent > 127 {
            sign | FLOAT_INFINITY
        } else if exponent < -126 {
            sign
        } else {
            sign | make_biased_exponent(exponent) | mantissa
        }
    }

    /// |Fx + Fy| and |Fx - Fy|.
    pub fn compute_fadd_abs(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let r = self.fadd_bits(fx, fy) & !FLOAT_SIGN_MASK;
        // The sign is stripped after the add, so re-derive the zero flag: an
        // exact cancellation leaves +0 rather than the signed zero fadd gave.
        self.astat &= !AN;
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fsub_abs(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let r = self.fsub_bits(fx, fy) & !FLOAT_SIGN_MASK;
        self.astat &= !AN;
        self.r[fn_] = r;
        self.astat |= AF;
    }

    /// Rn = FIX Fx BY Ry -- convert to integer after scaling by 2^Ry.
    pub fn compute_fix_scaled(&mut self, rn: usize, fx: usize, ry: usize) {
        let scaled = self.scalb_bits(self.r[fx], ry);
        let f = f32::from_bits(scaled);
        let alu_i = if self.mode1 & MODE1_TRUNCATE != 0 {
            f.floor() as i32
        } else {
            round_nearest(f) as i32
        } as u32;
        let src = self.r[fx];
        self.clear_alu_flags();
        self.flag_an(alu_i);
        self.flag_az(alu_i);
        if is_float_denormal(scaled) {
            self.stky |= AUS;
        }
        if is_float_nan(src) {
            self.astat |= AI;
        }
        self.r[rn] = alu_i;
        self.astat |= AF;
    }

    /// Fn = FLOAT Rx BY Ry -- convert from integer, then scale by 2^Ry.
    pub fn compute_float_scaled(&mut self, fn_: usize, rx: usize, ry: usize) {
        let x = ((self.r[rx] as i32) as f32).to_bits();
        self.clear_alu_flags();
        let r = self.scalb_bits(x, ry);
        self.flag_an(r);
        if is_float_denormal_or_zero(r) {
            self.astat |= AZ;
        }
        if is_float_denormal(r) {
            self.stky |= AUS;
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }

    /// Fixed-point dual add/subtract: Ra = Rx + Ry and Rs = Rx - Ry together.
    pub fn compute_dual_add_sub(&mut self, ra: usize, rs: usize, rx: usize, ry: usize) {
        let (x, y) = (self.r[rx], self.r[ry]);
        let mut r_add = x.wrapping_add(y);
        let mut r_sub = x.wrapping_sub(y);
        let av_add = !(x ^ y) & (x ^ r_add) & 0x8000_0000 != 0;
        let av_sub = (x ^ y) & (x ^ r_sub) & 0x8000_0000 != 0;

        self.clear_alu_flags();
        if av_add || av_sub {
            self.astat |= AV;
            self.stky |= AOS;
        }
        if r_add < x || r_sub <= x {
            self.astat |= AC;
        }
        if self.alusat() {
            if av_add {
                r_add = self.saturate(r_add);
            }
            if av_sub {
                r_sub = self.saturate(r_sub);
            }
        }
        if r_add == 0 || r_sub == 0 {
            self.astat |= AZ;
        }
        if (r_add | r_sub) & 0x8000_0000 != 0 {
            self.astat |= AN;
        }
        self.r[ra] = r_add;
        self.r[rs] = r_sub;
        self.astat &= !AF;
    }

    /// Floating-point dual add/subtract.
    pub fn compute_dual_fadd_fsub(&mut self, fa: usize, fs: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let add = self.fadd_bits(fx, fy);
        let sub = self.fsub_bits(fx, fy);
        self.r[fa] = add;
        self.r[fs] = sub;
    }

    // --- multiplier accumulate ---

    /// Rn = MRF + Rx * Ry (signed, integer).
    pub fn compute_mrf_plus_mul_ssin(&mut self, rn: usize, rx: usize, ry: usize) {
        let prod = (self.r[rx] as i32 as i64).wrapping_mul(self.r[ry] as i32 as i64);
        let r = (self.mrf as i64).wrapping_add(prod) as u64;
        self.clear_mul_flags();
        self.mul_flags(r);
        self.r[rn] = r as u32;
    }

    /// Rn = MRB + Rx * Ry (signed, integer).
    pub fn compute_mrb_plus_mul_ssin(&mut self, rn: usize, rx: usize, ry: usize) {
        let prod = (self.r[rx] as i32 as i64).wrapping_mul(self.r[ry] as i32 as i64);
        let r = (self.mrb as i64).wrapping_add(prod) as u64;
        self.clear_mul_flags();
        self.mul_flags(r);
        self.r[rn] = r as u32;
    }

    /// MR transfers. MR2F/MR2B are the 16-bit overflow halves, which the
    /// Model 2 microcode never touches; the reference calls them a fatal error, so
    /// leave them alone rather than corrupting the accumulator.
    pub fn compute_multi_mr_to_reg(&mut self, ai: usize, rk: usize) {
        match ai {
            0 => self.r[rk] = self.mrf as u32,
            1 => self.r[rk] = (self.mrf >> 32) as u32,
            4 => self.r[rk] = self.mrb as u32,
            5 => self.r[rk] = (self.mrb >> 32) as u32,
            _ => {}
        }
        self.clear_mul_flags();
    }

    pub fn compute_multi_reg_to_mr(&mut self, ai: usize, rk: usize) {
        let v = self.r[rk] as u64;
        match ai {
            0 => self.mrf = (self.mrf & !0xffff_ffff) | v,
            1 => self.mrf = (self.mrf & 0xffff_ffff) | (v << 32),
            4 => self.mrb = (self.mrb & !0xffff_ffff) | v,
            5 => self.mrb = (self.mrb & 0xffff_ffff) | (v << 32),
            _ => {}
        }
        self.clear_mul_flags();
    }

    // --- multi-function: fixed-point multiply with a parallel add/subtract ---

    fn mul_ssfr(&mut self, rxm: usize, rym: usize) -> u32 {
        let p = (self.r[rxm] as i32 as i64).wrapping_mul(self.r[rym] as i32 as i64);
        (p >> 31) as u32
    }

    pub fn compute_mul_ssfr_add(
        &mut self,
        rm: usize,
        rxm: usize,
        rym: usize,
        ra: usize,
        rxa: usize,
        rya: usize,
    ) {
        let r_mul = self.mul_ssfr(rxm, rym);
        let (x, y) = (self.r[rxa], self.r[rya]);
        let r_add = x.wrapping_add(y);
        self.clear_mul_flags();
        if r_mul & 0x8000_0000 != 0 {
            self.astat |= MN;
        }
        self.clear_alu_flags();
        self.flag_an(r_add);
        self.flag_az(r_add);
        self.flag_av_add(r_add, x, y);
        self.flag_ac_add(r_add, x);
        self.r[rm] = r_mul;
        self.r[ra] = r_add;
        self.astat &= !AF;
    }

    pub fn compute_mul_ssfr_sub(
        &mut self,
        rm: usize,
        rxm: usize,
        rym: usize,
        ra: usize,
        rxa: usize,
        rya: usize,
    ) {
        let r_mul = self.mul_ssfr(rxm, rym);
        let (x, y) = (self.r[rxa], self.r[rya]);
        let r_sub = x.wrapping_sub(y);
        self.clear_mul_flags();
        if r_mul & 0x8000_0000 != 0 {
            self.astat |= MN;
        }
        self.clear_alu_flags();
        self.flag_an(r_sub);
        self.flag_az(r_sub);
        self.flag_av_sub(r_sub, x, y);
        self.flag_ac_sub(r_sub, x);
        self.r[rm] = r_mul;
        self.r[ra] = r_sub;
        self.astat &= !AF;
    }

    // --- multi-function: float multiply with a parallel ALU operation ---

    pub fn compute_fmul_float_scaled(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        rxa: usize,
        rya: usize,
    ) {
        self.clear_mul_flags();
        self.clear_alu_flags();
        let m = self.fmul_bits(fxm, fym);
        let x = ((self.r[rxa] as i32) as f32).to_bits();
        let a = self.scalb_bits(x, rya);
        if f32::from_bits(a) < 0.0 {
            self.astat |= AN;
        }
        if is_float_denormal_or_zero(a) {
            self.astat |= AZ;
        }
        if is_float_denormal(a) {
            self.stky |= AUS;
        }
        self.r[fm] = m;
        self.r[fa] = a;
        self.astat |= AF;
    }

    pub fn compute_fmul_fix_scaled(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        ra: usize,
        fxa: usize,
        rya: usize,
    ) {
        self.clear_mul_flags();
        self.clear_alu_flags();
        let m = self.fmul_bits(fxm, fym);
        let src = self.r[fxa];
        let scaled = self.scalb_bits(src, rya);
        let f = f32::from_bits(scaled);
        // This one truncates toward zero rather than flooring.
        let alu_i = if self.mode1 & MODE1_TRUNCATE != 0 {
            f as i32
        } else {
            round_nearest(f) as i32
        } as u32;
        self.flag_an(alu_i);
        self.flag_az(alu_i);
        if is_float_denormal(scaled) {
            self.stky |= AUS;
        }
        if is_float_nan(src) {
            self.astat |= AI;
        }
        self.r[fm] = m;
        self.r[ra] = alu_i;
        self.astat |= AF;
    }

    pub fn compute_fmul_favg(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        let m = self.fmul_bits(fxm, fym);
        self.compute_favg(fa, fxa, fya);
        self.r[fm] = m;
    }

    pub fn compute_fmul_fabs(&mut self, fm: usize, fxm: usize, fym: usize, fa: usize, fxa: usize) {
        self.clear_mul_flags();
        let m = self.fmul_bits(fxm, fym);
        self.compute_fabs(fa, fxa);
        self.r[fm] = m;
    }

    pub fn compute_fmul_fmax(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        let m = self.fmul_bits(fxm, fym);
        self.compute_fmax(fa, fxa, fya);
        self.r[fm] = m;
    }

    pub fn compute_fmul_fmin(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        let m = self.fmul_bits(fxm, fym);
        self.compute_fmin(fa, fxa, fya);
        self.r[fm] = m;
    }

    // --- shifter operations the reference throws on, from the ADSP-2106x
    // --- manual: arithmetic shifts, plain FDEP and bit toggle.

    /// ASHIFT Rx BY Ry -- arithmetic, so a right shift keeps the sign.
    pub fn compute_ashift(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry] as i32;
        let v = if shift < 0 {
            if shift > -32 {
                ((self.r[rx] as i32) >> -shift) as u32
            } else if self.r[rx] & 0x8000_0000 != 0 {
                0xffff_ffff
            } else {
                0
            }
        } else {
            let v = if shift < 32 { self.r[rx] << shift } else { 0 };
            if shift > 0 {
                self.shift_sv();
            }
            v
        };
        self.r[rn] = v;
        self.shift_sz(v);
    }

    /// Rn = Rn OR ASHIFT Rx BY Ry
    pub fn compute_or_ashift(&mut self, rn: usize, rx: usize, ry: usize) {
        let prev = self.r[rn];
        self.compute_ashift(rn, rx, ry);
        let v = prev | self.r[rn];
        self.r[rn] = v;
        self.astat &= !crate::consts::SZ;
        self.shift_sz(v);
    }

    /// FDEP Rx BY Ry -- deposit without merging into the destination.
    pub fn compute_fdep(&mut self, rn: usize, rx: usize, ry: usize, signed: bool) {
        self.shift_begin();
        let bit = self.r[ry] & 0x3f;
        let len = (self.r[ry] >> 6) & 0x3f;
        let v = if len == 0 || bit >= 32 {
            0
        } else {
            let n = len.min(32);
            let field = if signed {
                sign_extend(self.r[rx] & mask_bits(n), n)
            } else {
                self.r[rx] & mask_bits(n)
            };
            field << bit
        };
        self.r[rn] = v;
        self.shift_sz(v);
        if bit + len > 32 {
            self.shift_sv();
        }
    }

    /// BTGL Rx BY Ry
    pub fn compute_btgl(&mut self, rn: usize, rx: usize, ry: usize) {
        self.shift_begin();
        let shift = self.r[ry];
        let mut v = self.r[rx];
        if shift < 32 {
            v ^= 1 << shift;
        } else {
            self.shift_sv();
        }
        self.r[rn] = v;
        self.shift_sz(v);
    }

    pub fn compute_and(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = self.r[rx] & self.r[ry];
        self.logic(rn, r);
    }
    pub fn compute_or(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = self.r[rx] | self.r[ry];
        self.logic(rn, r);
    }
    pub fn compute_xor(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = self.r[rx] ^ self.r[ry];
        self.logic(rn, r);
    }
    pub fn compute_not(&mut self, rn: usize, rx: usize) {
        let r = !self.r[rx];
        self.logic(rn, r);
    }
    pub fn compute_min(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = (self.r[rx] as i32).min(self.r[ry] as i32) as u32;
        self.logic(rn, r);
    }
    pub fn compute_max(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = (self.r[rx] as i32).max(self.r[ry] as i32) as u32;
        self.logic(rn, r);
    }
    pub fn compute_clip(&mut self, rn: usize, rx: usize, ry: usize) {
        let absry = (self.r[ry] as i32).abs();
        let r = (self.r[rx] as i32).clamp(-absry, absry) as u32;
        self.logic(rn, r);
    }

    // --- float ALU ops ---
    pub fn compute_fadd(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        self.r[fn_] = self.fadd_bits(fx, fy);
    }
    pub fn compute_fsub(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        self.r[fn_] = self.fsub_bits(fx, fy);
    }
    pub fn compute_fmul(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_mul_flags();
        self.r[fn_] = self.fmul_bits(fx, fy);
    }
    pub fn compute_fpass(&mut self, fn_: usize, fx: usize) {
        self.clear_alu_flags();
        let a = self.r[fx];
        let r;
        if is_float_nan(a) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            if is_float_denormal_or_zero(a) {
                r = a & FLOAT_SIGN_MASK;
                self.astat |= AZ;
            } else {
                r = a;
            }
            if r & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fneg(&mut self, fn_: usize, fx: usize) {
        self.clear_alu_flags();
        let a = self.r[fx];
        let r;
        if is_float_nan(a) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            if is_float_denormal_or_zero(a) {
                r = !a & FLOAT_SIGN_MASK;
                self.astat |= AZ;
            } else {
                r = a ^ FLOAT_SIGN_MASK;
            }
            if r & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fabs(&mut self, fn_: usize, fx: usize) {
        self.clear_alu_flags();
        let a = self.r[fx];
        let r;
        if is_float_nan(a) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            if is_float_denormal_or_zero(a) {
                r = 0;
                self.astat |= AZ;
            } else {
                r = a & !FLOAT_SIGN_MASK;
            }
            if a & FLOAT_SIGN_MASK != 0 {
                self.astat |= AS;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fcomp(&mut self, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        if is_float_nan(a) || is_float_nan(b) {
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let (xf, yf) = (f32::from_bits(a), f32::from_bits(b));
            if xf == yf {
                self.astat |= AZ;
            } else if xf < yf {
                self.astat |= AN;
            }
        }
        let mut comp_accum = (self.astat >> 1) & 0x7f00_0000;
        if self.astat & (AZ | AN) == 0 {
            comp_accum |= 0x8000_0000;
        }
        self.astat &= 0x00ff_ffff;
        self.astat |= comp_accum;
        self.astat |= AF;
    }
    pub fn compute_favg(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan_add(a, b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let x = f32::from_bits(flush_denormal(a));
            let y = f32::from_bits(flush_denormal(b));
            let mut rr = ((x + y) * 0.5f32).to_bits();
            if is_float_infinity(rr) {
                self.astat |= AV;
                self.stky |= AVS;
                if self.mode1 & MODE1_TRUNCATE != 0 {
                    rr = (rr & FLOAT_SIGN_MASK) | 0x7f7f_ffff;
                }
            } else if is_float_denormal_or_zero(rr) {
                self.astat |= AZ;
                if rr & FLOAT_MANTISSA_MASK != 0 {
                    self.stky |= AUS;
                }
                rr &= FLOAT_SIGN_MASK;
            }
            if rr & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
            r = rr;
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fmax(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan(a) || is_float_nan(b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let rf = f32::from_bits(a).max(f32::from_bits(b));
            r = rf.to_bits();
            if rf < 0.0 {
                self.astat |= AN;
            }
            if is_float_zero(r) {
                self.astat |= AZ;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fmin(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan(a) || is_float_nan(b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let rf = f32::from_bits(a).min(f32::from_bits(b));
            r = rf.to_bits();
            if rf < 0.0 {
                self.astat |= AN;
            }
            if is_float_zero(r) {
                self.astat |= AZ;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fclip(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan(a) || is_float_nan(b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let mut rr;
            if is_float_denormal_or_zero(a) || is_float_denormal_or_zero(b) {
                rr = a & FLOAT_SIGN_MASK;
                self.astat |= AZ;
            } else {
                let absry = f32::from_bits(b & !FLOAT_SIGN_MASK);
                let negabsry = f32::from_bits(b | FLOAT_SIGN_MASK);
                rr = f32::from_bits(a).clamp(negabsry, absry).to_bits();
                if is_float_zero(rr) {
                    self.astat |= AZ;
                }
            }
            if rr & FLOAT_SIGN_MASK != 0 {
                self.astat |= AN;
            }
            r = rr;
            let _ = &mut rr;
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_fcopysign(&mut self, fn_: usize, fx: usize, fy: usize) {
        self.clear_alu_flags();
        let (a, b) = (self.r[fx], self.r[fy]);
        let r;
        if is_float_nan(a) || is_float_nan(b) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let mut rr = b & FLOAT_SIGN_MASK;
            if rr != 0 {
                self.astat |= AN;
            }
            if is_float_denormal_or_zero(a) {
                self.astat |= AZ;
            } else {
                rr |= a & !FLOAT_SIGN_MASK;
            }
            r = rr;
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_scalb(&mut self, fn_: usize, fx: usize, ry: usize) {
        self.clear_alu_flags();
        let a = self.r[fx];
        if is_float_nan(a) {
            self.astat |= AI;
            self.stky |= AIS;
            self.r[fn_] = FLOAT_CANONICAL_NAN;
        } else {
            let mantissa = a & FLOAT_MANTISSA_MASK;
            let sign = a & FLOAT_SIGN_MASK;
            let mut exponent = get_unbiased_exponent(a) + self.r[ry] as i32;
            let r = if exponent > 127 {
                self.astat |= AV;
                sign | FLOAT_INFINITY
            } else if exponent < -126 {
                self.astat |= AZ;
                sign
            } else {
                let _ = &mut exponent;
                sign | make_biased_exponent(exponent) | mantissa
            };
            self.flag_an(r);
            if is_float_zero(r) {
                self.astat |= AZ;
            }
            if is_float_denormal(r) {
                self.stky |= AUS;
            }
            self.r[fn_] = r;
        }
        self.astat |= AF;
    }
    pub fn compute_logb(&mut self, rn: usize, fx: usize) {
        let a = self.r[fx];
        self.clear_alu_flags();
        if is_float_infinity(a) {
            self.r[rn] = FLOAT_INFINITY;
            self.astat |= AV;
        } else if is_float_zero(a) {
            self.r[rn] = FLOAT_SIGN_MASK | FLOAT_INFINITY;
            self.astat |= AV;
        } else if is_float_nan(a) {
            self.r[rn] = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else {
            let e = get_unbiased_exponent(a) as u32;
            self.flag_an(e);
            self.flag_az(e);
            self.r[rn] = e;
        }
        self.astat |= AF;
    }
    pub fn compute_float(&mut self, fn_: usize, rx: usize) {
        let r = ((self.r[rx] as i32) as f32).to_bits();
        self.r[fn_] = r;
        self.clear_alu_flags();
        self.flag_an(r);
        if is_float_denormal_or_zero(r) {
            self.astat |= AZ;
        }
        if is_float_denormal(r) {
            self.stky |= AUS;
        }
        self.astat |= AF;
    }
    pub fn compute_fix(&mut self, rn: usize, fx: usize) {
        let a = self.r[fx];
        let f = f32::from_bits(a);
        let alu_i = if self.mode1 & MODE1_TRUNCATE != 0 {
            f.floor() as i32
        } else {
            round_nearest(f) as i32
        } as u32;
        self.clear_alu_flags();
        self.flag_an(alu_i);
        self.flag_az(alu_i);
        if is_float_denormal(a) {
            self.stky |= AUS;
        }
        if is_float_nan(a) {
            self.astat |= AI;
        }
        self.r[rn] = alu_i;
        self.astat |= AF;
    }
    pub fn compute_recips(&mut self, fn_: usize, fx: usize) {
        self.clear_alu_flags();
        let a = self.r[fx];
        let r;
        if is_float_nan(a) {
            r = FLOAT_CANONICAL_NAN;
            self.astat |= AI;
            self.stky |= AIS;
        } else if is_float_zero(a) {
            r = (a & FLOAT_SIGN_MASK) | FLOAT_INFINITY;
            self.astat |= AV;
        } else {
            let mantissa = a & FLOAT_MANTISSA_MASK;
            let sign = a & FLOAT_SIGN_MASK;
            let mut res_exponent = -get_unbiased_exponent(a) - 1;
            let mut res_mantissa = RECIPS_MANTISSA_LOOKUP[(mantissa >> 16) as usize];
            if !(-126..=125).contains(&res_exponent) {
                res_exponent = 0;
                res_mantissa = 0;
            } else {
                res_exponent = (res_exponent + FLOAT_EXPONENT_BIAS) & 0xff;
            }
            r = sign | ((res_exponent as u32) << FLOAT_EXPONENT_SHIFT) | res_mantissa;
            self.flag_an(a);
            if is_float_zero(r) {
                self.astat |= AZ;
            }
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }
    pub fn compute_rsqrts(&mut self, fn_: usize, fx: usize) {
        let a = self.r[fx];
        let r = if a > 0x8000_0000 || is_float_nan(a) {
            FLOAT_CANONICAL_NAN
        } else {
            let mantissa = a & 0xffffff;
            let sign = a & FLOAT_SIGN_MASK;
            let res_exponent = -(get_unbiased_exponent(a) >> 1) - 1;
            let res_mantissa = RSQRTS_MANTISSA_LOOKUP[(mantissa >> 17) as usize];
            sign | make_biased_exponent(res_exponent) | res_mantissa
        };
        self.clear_alu_flags();
        if a == 0x8000_0000 {
            self.astat |= AN;
        }
        if is_float_zero(r) {
            self.astat |= AZ;
        }
        if is_float_zero(a) {
            self.astat |= AV;
        }
        if is_float_nan(a) || a & FLOAT_SIGN_MASK != 0 {
            self.astat |= AI;
        }
        if self.astat & AI != 0 {
            self.stky |= AIS;
        }
        self.r[fn_] = r;
        self.astat |= AF;
    }

    // --- multiplier ---
    pub fn compute_mul_uuin(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = (self.r[rx] as u64) * (self.r[ry] as u64);
        self.clear_mul_flags();
        self.mul_flags(r);
        self.r[rn] = r as u32;
    }
    pub fn compute_mul_ssin(&mut self, rn: usize, rx: usize, ry: usize) {
        let r = ((self.r[rx] as i32 as i64) * (self.r[ry] as i32 as i64)) as u64;
        self.clear_mul_flags();
        self.mul_flags(r);
        self.r[rn] = r as u32;
    }
    fn mul_flags(&mut self, r: u64) {
        if (r as u32) & 0x8000_0000 != 0 {
            self.astat |= MN;
        }
        let hi = (r >> 32) as u32;
        if hi != 0 && hi != 0xffff_ffff {
            self.astat |= MV;
        }
        if hi == 0 && (r as u32) != 0 {
            self.astat |= MU;
        }
    }

    // --- multi-function (parallel multiplier + ALU) ---
    pub fn compute_fmul_fadd(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        self.clear_alu_flags();
        let m = self.fmul_bits(fxm, fym);
        let a = self.fadd_bits(fxa, fya);
        self.r[fm] = m;
        self.r[fa] = a;
    }
    pub fn compute_fmul_fsub(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        self.clear_alu_flags();
        let m = self.fmul_bits(fxm, fym);
        let a = self.fsub_bits(fxa, fya);
        self.r[fm] = m;
        self.r[fa] = a;
    }
    // The multifunction opcodes name every operand register the instruction
    // encodes; grouping them would only hide which field is which.
    #[allow(clippy::too_many_arguments)]
    pub fn compute_fmul_dual_fadd_fsub(
        &mut self,
        fm: usize,
        fxm: usize,
        fym: usize,
        fa: usize,
        fs: usize,
        fxa: usize,
        fya: usize,
    ) {
        self.clear_mul_flags();
        self.clear_alu_flags();
        let m = self.fmul_bits(fxm, fym);
        let add = self.fadd_bits(fxa, fya);
        // FSUB reuses the flags path; recompute the subtract independently.
        let sub = self.fsub_bits(fxa, fya);
        self.r[fm] = m;
        self.r[fa] = add;
        self.r[fs] = sub;
    }

    /// COMPUTE: the compute-field dispatcher (opcode bits 0..22). Returns false
    /// for a compute operation not yet ported, so the caller can count it.
    pub fn compute(&mut self, opcode: u32) -> bool {
        let op = ((opcode >> 12) & 0xff) as usize;
        let cu = (opcode >> 20) & 0x3;
        let rn = ((opcode >> 8) & 0xf) as usize;
        let rx = ((opcode >> 4) & 0xf) as usize;
        let ry = (opcode & 0xf) as usize;

        if opcode & 0x40_0000 != 0 {
            // multi-function
            let fm = ((opcode >> 12) & 0xf) as usize;
            let fa = ((opcode >> 8) & 0xf) as usize;
            let fxm = ((opcode >> 6) & 0x3) as usize;
            let fym = (((opcode >> 4) & 0x3) + 4) as usize;
            let fxa = (((opcode >> 2) & 0x3) + 8) as usize;
            let fya = ((opcode & 0x3) + 12) as usize;
            let multiop = (opcode >> 16) & 0x3f;
            match multiop {
                0x00 => self.compute_multi_mr_to_reg(op & 0xf, rn),
                0x01 => self.compute_multi_reg_to_mr(op & 0xf, rn),
                0x04 => self.compute_mul_ssfr_add(fm, fxm, fym, fa, fxa, fya),
                0x05 => self.compute_mul_ssfr_sub(fm, fxm, fym, fa, fxa, fya),
                0x18 => self.compute_fmul_fadd(fm, fxm, fym, fa, fxa, fya),
                0x19 => self.compute_fmul_fsub(fm, fxm, fym, fa, fxa, fya),
                0x1a => self.compute_fmul_float_scaled(fm, fxm, fym, fa, fxa, fya),
                0x1b => self.compute_fmul_fix_scaled(fm, fxm, fym, fa, fxa, fya),
                0x1c => self.compute_fmul_favg(fm, fxm, fym, fa, fxa, fya),
                0x1d => self.compute_fmul_fabs(fm, fxm, fym, fa, fxa),
                0x1e => self.compute_fmul_fmax(fm, fxm, fym, fa, fxa, fya),
                0x1f => self.compute_fmul_fmin(fm, fxm, fym, fa, fxa, fya),
                0x30..=0x3f => {
                    let fs = ((opcode >> 16) & 0xf) as usize;
                    self.compute_fmul_dual_fadd_fsub(fm, fxm, fym, fa, fs, fxa, fya);
                }
                _ => return false,
            }
            return true;
        }

        match cu {
            0 => match op {
                0x01 => self.compute_add(rn, rx, ry),
                0x02 => self.compute_sub(rn, rx, ry),
                0x05 => self.compute_add_ci(rn, rx, ry),
                0x06 => self.compute_sub_ci(rn, rx, ry),
                0x0a => self.compute_comp(rx, ry),
                0x21 => self.compute_pass(rn, rx),
                0x22 => self.compute_neg(rn, rx),
                0x25 => self.compute_add_ci1(rn, rx),
                0x26 => self.compute_sub_ci1(rn, rx),
                0x29 => self.compute_inc(rn, rx),
                0x2a => self.compute_dec(rn, rx),
                0x30 => self.compute_abs(rn, rx),
                0x40 => self.compute_and(rn, rx, ry),
                0x41 => self.compute_or(rn, rx, ry),
                0x42 => self.compute_xor(rn, rx, ry),
                0x43 => self.compute_not(rn, rx),
                0x61 => self.compute_min(rn, rx, ry),
                0x62 => self.compute_max(rn, rx, ry),
                0x63 => self.compute_clip(rn, rx, ry),
                0x81 => self.compute_fadd(rn, rx, ry),
                0x82 => self.compute_fsub(rn, rx, ry),
                0x89 => self.compute_favg(rn, rx, ry),
                0x8a => self.compute_fcomp(rx, ry),
                0xa1 => self.compute_fpass(rn, rx),
                0xa2 => self.compute_fneg(rn, rx),
                0xb0 => self.compute_fabs(rn, rx),
                0xbd => self.compute_scalb(rn, rx, ry),
                0xc1 => self.compute_logb(rn, rx),
                0xc4 => self.compute_recips(rn, rx),
                0xc5 => self.compute_rsqrts(rn, rx),
                0xc9 => self.compute_fix(rn, rx),
                0xca => self.compute_float(rn, rx),
                0xe1 => self.compute_fmin(rn, rx, ry),
                0xe2 => self.compute_fmax(rn, rx, ry),
                0xe3 => self.compute_fclip(rn, rx, ry),
                0xe0 => self.compute_fcopysign(rn, rx, ry),
                0x91 => self.compute_fadd_abs(rn, rx, ry),
                0x92 => self.compute_fsub_abs(rn, rx, ry),
                0xd9 => self.compute_fix_scaled(rn, rx, ry),
                0xda => self.compute_float_scaled(rn, rx, ry),
                // Dual add/subtract writes two registers named by their own
                // fields in the opcode rather than by rn.
                0x70..=0x7f => {
                    let rs = ((opcode >> 12) & 0xf) as usize;
                    let ra = ((opcode >> 8) & 0xf) as usize;
                    self.compute_dual_add_sub(ra, rs, rx, ry);
                }
                0xf0..=0xff => {
                    let rs = ((opcode >> 12) & 0xf) as usize;
                    let ra = ((opcode >> 8) & 0xf) as usize;
                    self.compute_dual_fadd_fsub(ra, rs, rx, ry);
                }
                _ => return false,
            },
            1 => match op {
                0x14 => self.mrf = 0,
                0x16 => self.mrb = 0,
                0x30 => self.compute_fmul(rn, rx, ry),
                0x40 => self.compute_mul_uuin(rn, rx, ry),
                0x70 => self.compute_mul_ssin(rn, rx, ry),
                0xb0 => self.compute_mrf_plus_mul_ssin(rn, rx, ry),
                0xb2 => self.compute_mrb_plus_mul_ssin(rn, rx, ry),
                _ => return false,
            },
            2 => match op >> 2 {
                0x00 => self.compute_lshift(rn, rx, ry),
                0x02 => self.compute_rot(rn, rx, ry),
                0x08 => self.compute_or_lshift(rn, rx, ry),
                0x10 => self.compute_fext(rn, rx, ry, false),
                0x12 => self.compute_fext(rn, rx, ry, true),
                0x19 => self.compute_or_fdep(rn, rx, ry),
                0x30 => self.compute_bit_set(rn, rx, ry, true),
                0x31 => self.compute_bit_set(rn, rx, ry, false),
                0x33 => self.compute_btst(rx, ry),
                0x01 => self.compute_ashift(rn, rx, ry),
                0x09 => self.compute_or_ashift(rn, rx, ry),
                0x11 => self.compute_fdep(rn, rx, ry, false),
                0x13 => self.compute_fdep(rn, rx, ry, true),
                0x32 => self.compute_btgl(rn, rx, ry),
                _ => return false,
            },
            _ => return false,
        }
        true
    }
}

/// `n` low bits set; `n` may be 32, which a plain shift cannot express.
#[inline]
fn mask_bits(n: u32) -> u32 {
    if n >= 32 {
        u32::MAX
    } else {
        (1u32 << n) - 1
    }
}

/// Sign-extends the low `n` bits of `v`.
#[inline]
fn sign_extend(v: u32, n: u32) -> u32 {
    if n == 0 || n >= 32 {
        return v;
    }
    let sh = 32 - n;
    (((v << sh) as i32) >> sh) as u32
}

/// Round to nearest, ties to even -- matches nearbyintf under FE_TONEAREST.
fn round_nearest(f: f32) -> f32 {
    let r = f.round();
    if (f - f.trunc()).abs() == 0.5 {
        // round half to even
        let lower = f.floor();
        if (lower as i64) % 2 == 0 {
            lower
        } else {
            lower + 1.0
        }
    } else {
        r
    }
}

impl Sharc {
    /// SHIFT_OPERATION_IMM: the shifter unit with an immediate operand.
    /// Returns false for a sub-op that is not implemented.
    pub fn shift_imm(&mut self, shiftop: u32, data: i32, rn: usize, rx: usize) -> bool {
        let shift = data as i8 as i32; // low 8 bits, signed
        let bit = (data & 0x3f) as u32;
        let len = ((data >> 6) & 0x3f) as u32;

        self.astat &= !(SZ | SV | SS);
        let set_sz = |s: &mut Self, v: u32| {
            if v == 0 {
                s.astat |= SZ;
            }
        };

        match shiftop {
            0x00 => {
                // LSHIFT Rx BY <data8>
                let v = if shift < 0 {
                    if shift > -32 {
                        self.r[rx] >> -shift
                    } else {
                        0
                    }
                } else {
                    let v = if shift < 32 { self.r[rx] << shift } else { 0 };
                    if shift > 0 {
                        self.astat |= SV;
                    }
                    v
                };
                self.r[rn] = v;
                set_sz(self, v);
            }
            0x01 => {
                // ASHIFT Rx BY <data8>
                let v = if shift < 0 {
                    ((self.r[rx] as i32) >> if shift > -32 { -shift } else { 31 }) as u32
                } else {
                    let v = if shift < 32 {
                        ((self.r[rx] as i32) << shift) as u32
                    } else {
                        0
                    };
                    if shift > 0 {
                        self.astat |= SV;
                    }
                    v
                };
                self.r[rn] = v;
                set_sz(self, v);
            }
            0x02 => {
                // ROT Rx BY <data8>
                let v = self.r[rx].rotate_left((shift & 31) as u32);
                self.r[rn] = v;
                set_sz(self, v);
            }
            0x08 => {
                // Rn = Rn OR LSHIFT Rx BY <data8>
                let r = if shift < 0 {
                    if shift > -32 {
                        self.r[rx] >> -shift
                    } else {
                        0
                    }
                } else {
                    let v = if shift < 32 { self.r[rx] << shift } else { 0 };
                    if shift > 0 {
                        self.astat |= SV;
                    }
                    v
                };
                set_sz(self, r);
                self.r[rn] |= r;
            }
            0x10 => {
                // FEXT Rx BY <bit6>:<len6>
                let v = if len == 0 || bit >= 32 {
                    0
                } else {
                    let l = len.min(32);
                    let mask = if l >= 32 { u32::MAX } else { (1u32 << l) - 1 };
                    (self.r[rx] >> bit) & mask
                };
                self.r[rn] = v;
                set_sz(self, v);
                if bit + len > 32 {
                    self.astat |= SV;
                }
            }
            0x11 => {
                // Rn = Rn FDEP Rx BY <bit6>:<len6>
                let v = if len == 0 || bit >= 32 {
                    0
                } else {
                    let l = len.min(32);
                    let mask = if l >= 32 { u32::MAX } else { (1u32 << l) - 1 };
                    (self.r[rx] & mask) << bit
                };
                self.r[rn] = v;
                set_sz(self, v);
                if bit + len > 32 {
                    self.astat |= SV;
                }
            }
            0x12 => {
                // FEXT Rx BY <bit6>:<len6>, sign extended
                let v = if len == 0 || bit >= 32 {
                    0
                } else if bit + len > 32 {
                    self.r[rx] >> bit
                } else {
                    let l = len.min(32);
                    sext32(self.r[rx] >> bit, l)
                };
                self.r[rn] = v;
                set_sz(self, v);
                if bit + len > 32 {
                    self.astat |= SV;
                }
            }
            0x13 => {
                // FDEP Rx BY <bit6>:<len6>, sign extended
                let v = if len == 0 || bit >= 32 {
                    0
                } else {
                    sext32(self.r[rx], len.min(32)) << bit
                };
                self.r[rn] = v;
                set_sz(self, v);
                if bit + len > 32 {
                    self.astat |= SV;
                }
            }
            0x19 => {
                // Rn = Rn OR FDEP Rx BY <bit6>:<len6>
                if len != 0 && bit < 32 {
                    let l = len.min(32);
                    let mask = if l >= 32 { u32::MAX } else { (1u32 << l) - 1 };
                    self.r[rn] |= (self.r[rx] & mask) << bit;
                }
                let v = self.r[rn];
                set_sz(self, v);
                if bit + len > 32 {
                    self.astat |= SV;
                }
            }
            0x30..=0x32 => {
                // BSET / BCLR / BTGL Rx BY <data8>
                let mut v = self.r[rx];
                if (0..32).contains(&data) {
                    let m = 1u32 << data;
                    v = match shiftop {
                        0x30 => v | m,
                        0x31 => v & !m,
                        _ => v ^ m,
                    };
                } else {
                    self.astat |= SV;
                }
                self.r[rn] = v;
                set_sz(self, v);
            }
            0x33 => {
                // BTST Rx BY <data8>
                if data < 32 {
                    let r = self.r[rx] & (1u32 << data);
                    set_sz(self, r);
                } else {
                    self.astat |= SZ | SV;
                }
            }
            _ => return false,
        }
        true
    }
}

/// Sign-extends the low `bits` of `v`.
#[inline]
fn sext32(v: u32, bits: u32) -> u32 {
    if bits == 0 || bits >= 32 {
        return v;
    }
    let sh = 32 - bits;
    (((v << sh) as i32) >> sh) as u32
}
