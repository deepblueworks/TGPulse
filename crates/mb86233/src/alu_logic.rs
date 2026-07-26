use crate::cpu_state::Mb86233;
use crate::types::*;

// --- Helper Functions for Bit-Casting ---
// In C++: *(float *)&u
// In Rust: f32::from_bits(u)

#[inline(always)]
fn u2f(u: u32) -> f32 {
    f32::from_bits(u)
}

#[inline(always)]
fn f2u(f: f32) -> u32 {
    f.to_bits()
}

impl Mb86233 {
    // --- Flag Helpers ---

    /// Sets the ZRD (Zero) and SGD (Sign) flags based on an Integer result.
    fn stset_set_sz_int(&mut self, val: u32) {
        if val != 0 {
            if (val & 0x80000000) != 0 {
                self.alu.stset = F_SGD;
            } else {
                self.alu.stset = 0;
            }
        } else {
            self.alu.stset = F_ZRD;
        }
    }

    /// Sets the ZRD (Zero) and SGD (Sign) flags based on a Floating Point result.
    /// Checks bits directly to avoid +0.0 vs -0.0 issues if logic requires strict bit adherence,
    /// though standard IEEE behavior usually treats +0/-0 as zero.
    /// The reference logic: `(val & 0x7fffffff) ? (val & 0x80000000 ? F_SGD : 0) : F_ZRD`
    fn stset_set_sz_fp(&mut self, val: u32) {
        if (val & 0x7fffffff) != 0 {
            if (val & 0x80000000) != 0 {
                self.alu.stset = F_SGD;
            } else {
                self.alu.stset = 0;
            }
        } else {
            self.alu.stset = F_ZRD;
        }
    }

    /// Commit the calculated flags to the main Status Register (ST).
    pub fn alu_update_st(&mut self) {
        self.st = (self.st & !self.alu.stmask) | self.alu.stset;
    }

    // --- Main ALU Logic ---

    /// Pre-calculation phase: Calculates result and flags but does NOT write to D/P registers yet.
    /// Corresponds to `mb86233_device::alu_pre`
    pub fn alu_pre(&mut self, alu: u32) {
        match alu {
            0x00 => { /* No ALU operation */ }

            0x01 => {
                // andd
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d & self.a;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x02 => {
                // orad
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d | self.a;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x03 => {
                // eord
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d ^ self.a;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x04 => {
                // notd
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = !self.d;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x05 => {
                // fcpd (Float Compare: D - A)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                let r = f2u(u2f(self.d) - u2f(self.a));
                self.stset_set_sz_fp(r);
                // Note: fcpd updates flags but does NOT update D (handled in post_2)
            }

            0x06 => {
                // fadd (D + A)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) + u2f(self.a));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x07 => {
                // fsbd (D - A)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) - u2f(self.a));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x08 => {
                // fml (A * B) -> P
                self.alu.stmask = 0;
                self.alu.r1 = f2u(u2f(self.a) * u2f(self.b));
                self.alu.stset = 0;
            }

            0x09 => {
                // fmsd (D + P -> D, A * B -> P)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) + u2f(self.p)); // New D
                self.alu.r2 = f2u(u2f(self.a) * u2f(self.b)); // New P
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x0A => {
                // fmrd (D - P -> D, A * B -> P)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) - u2f(self.p)); // New D
                self.alu.r2 = f2u(u2f(self.a) * u2f(self.b)); // New P
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x0B => {
                // fabd (fabs(D))
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d & 0x7fffffff;
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x0C => {
                // fsmd (D + P)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) + u2f(self.p));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x0D => {
                // fspd (P -> D, A * B -> P)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.p; // New D
                self.alu.r2 = f2u(u2f(self.a) * u2f(self.b)); // New P
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x0E => {
                // cxfd (Int to Float)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                // Treat D as signed 32-bit int, convert to float
                self.alu.r1 = f2u((self.d as i32) as f32);
                // "stset_set_sz_int" is used in original code for cxfd result,
                // likely because the input was int, or checking result as bits.
                // The status flags are set from the ALU result here.
                self.stset_set_sz_int(self.alu.r1);
            }

            0x0F => {
                // cfxd (Float to Int)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                let val_f = u2f(self.d);
                let result_i32: i32 = match (self.m >> 1) & 3 {
                    0 => val_f.round() as i32, // to nearest
                    1 => val_f.ceil() as i32,  // up
                    2 => val_f.floor() as i32, // down
                    _ => val_f as i32,         // truncate
                };

                self.alu.r1 = result_i32 as u32;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x10 => {
                // fdvd (D / A)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.d) / u2f(self.a));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x11 => {
                // fned (Negate D)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = if self.d != 0 { self.d ^ 0x80000000 } else { 0 };
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x13 => {
                // d = b + a
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.b) + u2f(self.a));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x14 => {
                // d = b - a
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = f2u(u2f(self.b) - u2f(self.a));
                self.stset_set_sz_fp(self.alu.r1);
            }

            0x16 => {
                // lsrd (Logical Shift Right)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d >> self.sft;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x17 => {
                // lsld (Logical Shift Left)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d << self.sft;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x18 => {
                // asrd (Arithmetic Shift Right)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                // Cast to i32 for sign extension
                self.alu.r1 = ((self.d as i32) >> self.sft) as u32;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x19 => {
                // asld (Arithmetic Shift Left)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = ((self.d as i32) << self.sft) as u32;
                self.stset_set_sz_int(self.alu.r1);
            }

            0x1A => {
                // addd (Integer Add)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d.wrapping_add(self.a);
                self.stset_set_sz_int(self.alu.r1);
            }

            0x1B => {
                // subd (Integer Subtract)
                self.alu.stmask = F_ZRD | F_SGD | F_CPD | F_OVD | F_DVZD;
                self.alu.r1 = self.d.wrapping_sub(self.a);
                self.stset_set_sz_int(self.alu.r1);
            }

            _ => {
                self.cov.alu_unknown[(alu & 0x1f) as usize] += 1;
            }
        }
    }

    /// Post-calculation: Update D for immediate integer ops.
    /// Corresponds to `mb86233_device::alu_post_1`
    pub fn alu_post_1(&mut self, alu: u32) {
        match alu {
            0x01 | 0x02 | 0x03 | 0x04 | 0x0E | 0x0F | 0x16 | 0x17 | 0x18 | 0x19 | 0x1A | 0x1B => {
                // Update D register
                self.d = self.alu.r1;
                self.alu_update_st();
            }
            _ => {}
        }
    }

    /// Post-calculation: Update D/P for floating point ops (assumed 2 cycles).
    /// Corresponds to `mb86233_device::alu_post_2`
    pub fn alu_post_2(&mut self, alu: u32) {
        match alu {
            0x05 => {
                // fcpd: flags only
                self.alu_update_st();
                self.icount -= 1;
            }

            0x06 | 0x07 | 0x0B | 0x0C | 0x10 | 0x11 | 0x13 | 0x14 => {
                // Update D
                self.d = self.alu.r1;
                self.alu_update_st();
                self.icount -= 1;
            }

            0x08 => {
                // fml: Update P
                self.p = self.alu.r1;
                self.icount -= 1;
            }

            0x09 | 0x0A | 0x0D => {
                // fmsd/fmrd/fspd: Update D and P
                self.d = self.alu.r1;
                self.p = self.alu.r2;
                self.alu_update_st();
                self.icount -= 1;
            }

            _ => {}
        }
    }
}
