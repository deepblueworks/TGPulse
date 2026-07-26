//! Universal-register access, condition codes, and the PC / loop / status
//! stacks.

use crate::consts::*;
use crate::state::Sharc;

impl Sharc {
    // --- Register file / DAG bank helpers ---
    #[inline]
    pub fn reg(&self, x: usize) -> u32 {
        self.r[x]
    }
    #[inline]
    pub fn set_reg(&mut self, x: usize, v: u32) {
        self.r[x] = v;
    }
    #[inline]
    pub fn freg(&self, x: usize) -> f32 {
        f32::from_bits(self.r[x])
    }
    #[inline]
    pub fn set_freg(&mut self, x: usize, v: f32) {
        self.r[x] = v.to_bits();
    }

    /// GET_UREG: reads a universal register by its 8-bit index.
    pub fn get_ureg(&self, ureg: usize) -> u32 {
        let reg = ureg & 0xf;
        match (ureg >> 4) & 0xf {
            0x0 => self.r[reg],
            0x1 => {
                if reg & 0x8 != 0 {
                    self.dag2.i[reg & 0x7]
                } else {
                    self.dag1.i[reg & 0x7]
                }
            }
            0x2 => {
                if reg & 0x8 != 0 {
                    // M8-M15 are sign-extended from 24 bits.
                    let r = self.dag2.m[reg & 0x7];
                    if r & 0x800000 != 0 {
                        r | 0xff00_0000
                    } else {
                        r
                    }
                } else {
                    self.dag1.m[reg & 0x7]
                }
            }
            0x3 => {
                if reg & 0x8 != 0 {
                    self.dag2.l[reg & 0x7]
                } else {
                    self.dag1.l[reg & 0x7]
                }
            }
            0x4 => {
                if reg & 0x8 != 0 {
                    self.dag2.b[reg & 0x7]
                } else {
                    self.dag1.b[reg & 0x7]
                }
            }
            0x6 => match reg {
                0x4 => self.pcstk,
                0x5 => self.pcstkp,
                0x7 => self.curlcntr,
                0x8 => self.lcntr,
                _ => 0,
            },
            0x7 => match reg {
                0x0 => self.ustat1,
                0x1 => self.ustat2,
                0x9 => self.irptl,
                0xa => self.mode2,
                0xb => self.mode1,
                0xc => self.astat_with_flags(),
                0xd => self.imask,
                0xe => self.stky,
                _ => 0,
            },
            0xd => match reg {
                0xb => self.px as u32,
                0xc => (self.px & 0xffff) as u32,
                0xd => (self.px >> 16) as u32,
                _ => 0,
            },
            _ => 0,
        }
    }

    /// ASTAT read merges in the FLAG pin inputs.
    fn astat_with_flags(&self) -> u32 {
        let mut r = self.astat;
        let sel = (self.mode2 >> 15) & 0xf;
        r &= (sel << FLG0_SHIFT) | !(FLG0 | FLG1 | FLG2 | FLG3);
        for i in 0..4 {
            if (self.mode2 >> (i + 15)) & 1 == 0 {
                r |= self.flag[i] << (FLG0_SHIFT + i as u32);
            }
        }
        r
    }

    /// SET_UREG: writes a universal register by its 8-bit index.
    pub fn set_ureg(&mut self, ureg: usize, data: u32) {
        let reg = ureg & 0xf;
        match (ureg >> 4) & 0xf {
            0x0 => self.r[reg] = data,
            0x1 => {
                if reg & 0x8 != 0 {
                    self.dag2.i[reg & 0x7] = data;
                } else {
                    self.dag1.i[reg & 0x7] = data;
                }
            }
            0x2 => {
                if reg & 0x8 != 0 {
                    self.dag2.m[reg & 0x7] = data;
                } else {
                    self.dag1.m[reg & 0x7] = data;
                }
            }
            0x3 => {
                if reg & 0x8 != 0 {
                    self.dag2.l[reg & 0x7] = data;
                } else {
                    self.dag1.l[reg & 0x7] = data;
                }
            }
            0x4 => {
                // Loading B also loads I with the same value.
                if reg & 0x8 != 0 {
                    self.dag2.b[reg & 0x7] = data;
                    self.dag2.i[reg & 0x7] = data;
                } else {
                    self.dag1.b[reg & 0x7] = data;
                    self.dag1.i[reg & 0x7] = data;
                }
            }
            0x6 => match reg {
                0x7 => {
                    if self.lstkp > 0 && self.lstkp < 7 {
                        self.curlcntr = data;
                    }
                }
                0x8 if self.lstkp < 6 => {
                    self.lcntr = data;
                }
                _ => {}
            },
            0x7 => match reg {
                0x0 => self.ustat1 = data,
                0x1 => self.ustat2 = data,
                0x9 => self.irptl = data,
                0xa => self.mode2 = data,
                0xb => self.write_mode1(data),
                0xc => self.astat = data,
                0xd => self.imask = data,
                0xe => {
                    let keep = LSEM | LSOV | SSEM | SSOV | PCEM | PCFL;
                    self.stky = (self.stky & keep) | (data & !keep);
                }
                _ => {}
            },
            0xd => match reg {
                0xc => self.px = (self.px & 0xffff_ffff_ffff_0000) | (data as u64 & 0xffff),
                0xd => self.px = (self.px & 0x0000_0000_0000_ffff) | ((data as u64) << 16),
                _ => {}
            },
            _ => {}
        }
    }

    /// MODE1 write, applying the register/DAG-bank swaps its select bits toggle.
    fn write_mode1(&mut self, data: u32) {
        let diff = data ^ self.mode1;
        self.mode1 = data;
        if diff & MODE1_SRD1H != 0 {
            for k in 4..8 {
                std::mem::swap(&mut self.dag1.i[k], &mut self.dag1_alt.i[k]);
                std::mem::swap(&mut self.dag1.m[k], &mut self.dag1_alt.m[k]);
                std::mem::swap(&mut self.dag1.l[k], &mut self.dag1_alt.l[k]);
                std::mem::swap(&mut self.dag1.b[k], &mut self.dag1_alt.b[k]);
            }
        }
        if diff & MODE1_SRD1L != 0 {
            for k in 0..4 {
                std::mem::swap(&mut self.dag1.i[k], &mut self.dag1_alt.i[k]);
                std::mem::swap(&mut self.dag1.m[k], &mut self.dag1_alt.m[k]);
                std::mem::swap(&mut self.dag1.l[k], &mut self.dag1_alt.l[k]);
                std::mem::swap(&mut self.dag1.b[k], &mut self.dag1_alt.b[k]);
            }
        }
        if diff & MODE1_SRD2H != 0 {
            for k in 4..8 {
                std::mem::swap(&mut self.dag2.i[k], &mut self.dag2_alt.i[k]);
                std::mem::swap(&mut self.dag2.m[k], &mut self.dag2_alt.m[k]);
                std::mem::swap(&mut self.dag2.l[k], &mut self.dag2_alt.l[k]);
                std::mem::swap(&mut self.dag2.b[k], &mut self.dag2_alt.b[k]);
            }
        }
        if diff & MODE1_SRD2L != 0 {
            for k in 0..4 {
                std::mem::swap(&mut self.dag2.i[k], &mut self.dag2_alt.i[k]);
                std::mem::swap(&mut self.dag2.m[k], &mut self.dag2_alt.m[k]);
                std::mem::swap(&mut self.dag2.l[k], &mut self.dag2_alt.l[k]);
                std::mem::swap(&mut self.dag2.b[k], &mut self.dag2_alt.b[k]);
            }
        }
        if diff & MODE1_SRRFH != 0 {
            for k in 8..16 {
                std::mem::swap(&mut self.r[k], &mut self.reg_alt[k]);
            }
        }
        if diff & MODE1_SRRFL != 0 {
            for k in 0..8 {
                std::mem::swap(&mut self.r[k], &mut self.reg_alt[k]);
            }
        }
    }

    // --- Condition codes ---
    #[inline]
    fn cond_lt(&self) -> bool {
        if self.astat & AF != 0 {
            (self.astat & AN != 0) && (self.astat & AZ == 0)
        } else {
            (self.astat & AN != 0) != ((self.astat & AV != 0) && (self.mode1 & MODE1_ALUSAT == 0))
        }
    }
    #[inline]
    fn cond_le(&self) -> bool {
        (self.astat & AZ != 0)
            || if self.astat & AF != 0 {
                self.astat & AN != 0
            } else {
                (self.astat & AN != 0)
                    != ((self.astat & AV != 0) && (self.mode1 & MODE1_ALUSAT == 0))
            }
    }

    /// IF_CONDITION_CODE: the pre-instruction condition test.
    pub fn if_cond(&self, cond: u32) -> bool {
        match cond {
            0x00 => self.astat & AZ != 0,
            0x01 => self.cond_lt(),
            0x02 => self.cond_le(),
            0x03 => self.astat & AC != 0,
            0x04 => self.astat & AV != 0,
            0x05 => self.astat & MV != 0,
            0x06 => self.astat & MN != 0,
            0x07 => self.astat & SV != 0,
            0x08 => self.astat & SZ != 0,
            0x09 => self.flag[0] != 0,
            0x0a => self.flag[1] != 0,
            0x0b => self.flag[2] != 0,
            0x0c => self.flag[3] != 0,
            0x0d => self.astat & BTF != 0,
            0x0e => false,
            0x0f => self.curlcntr != 1, // NOT LCE
            0x10 => self.astat & AZ == 0,
            0x11 => !self.cond_lt(),
            0x12 => !self.cond_le(),
            0x13 => self.astat & AC == 0,
            0x14 => self.astat & AV == 0,
            0x15 => self.astat & MV == 0,
            0x16 => self.astat & MN == 0,
            0x17 => self.astat & SV == 0,
            0x18 => self.astat & SZ == 0,
            0x19 => self.flag[0] == 0,
            0x1a => self.flag[1] == 0,
            0x1b => self.flag[2] == 0,
            0x1c => self.flag[3] == 0,
            0x1d => self.astat & BTF == 0,
            0x1e => true,
            0x1f => true, // TRUE
            _ => true,
        }
    }

    /// DO_CONDITION_CODE: the loop-termination condition test (differs from
    /// IF only for LCE and the always-true/false slots).
    pub fn do_cond(&self, cond: u32) -> bool {
        match cond {
            0x0f => self.curlcntr == 1, // LCE
            0x1e => true,               // NOT BM
            0x1f => false,              // FOREVER
            _ => self.if_cond(cond),
        }
    }

    // --- PC stack ---
    //
    // `pcstk` is the live top-of-stack register; `pcstack` is its backing
    // store. Push archives the current top and bumps the pointer -- the caller
    // then writes the new top into `pcstk`. Ported from the published tables
    // PUSH_PC/POP_PC, whose asymmetry matters: a DO UNTIL pushes here too.
    pub fn push_pc_raw(&mut self) {
        if self.pcstkp >= 30 {
            return; // stack overflow; a real part would fault
        }
        if self.pcstkp > 0 {
            self.pcstack[(self.pcstkp - 1) as usize] = self.pcstk;
        }
        self.pcstkp += 1;
        self.stky &= !PCEM;
        if self.pcstkp >= 30 {
            self.stky |= PCFL;
        }
    }

    /// Pushes an explicit return address (the call forms).
    pub fn push_pc(&mut self, pc: u32) {
        self.push_pc_raw();
        self.pcstk = pc;
    }

    pub fn pop_pc(&mut self) -> u32 {
        if self.pcstkp == 0 {
            return self.pcstk; // stack underflow; a real part would fault
        }
        let result = self.pcstk;
        self.pcstkp -= 1;
        if self.pcstkp < 30 {
            self.pcstack[self.pcstkp as usize] = self.pcstk;
            if self.pcstkp > 0 {
                self.pcstk = self.pcstack[(self.pcstkp - 1) as usize];
            } else {
                self.pcstk = 0x00ff_ffff;
                self.stky |= PCEM;
            }
            self.stky &= !PCFL;
        }
        result
    }

    // --- Loop stack ---
    #[inline]
    fn laddr_pack(&self) -> u32 {
        (self.laddr_loop_type << 30) | (self.laddr_code << 24) | self.laddr_addr
    }
    #[inline]
    fn laddr_unpack(&mut self, v: u32) {
        self.laddr_addr = v & 0x00ff_ffff;
        self.laddr_code = (v >> 24) & 0x1f;
        self.laddr_loop_type = (v >> 30) & 0x3;
    }

    pub fn push_loop(&mut self) {
        if self.lstkp >= 6 {
            return; // overflow
        }
        if self.lstkp > 0 {
            self.lcstack[(self.lstkp - 1) as usize] = self.curlcntr;
            self.lastack[(self.lstkp - 1) as usize] = self.laddr_pack();
        }
        self.curlcntr = self.lcntr;
        let packed = self.lastack[self.lstkp as usize];
        self.laddr_unpack(packed);
        self.lstkp += 1;
        self.lcntr = if self.lstkp < 6 {
            self.lcstack[self.lstkp as usize]
        } else {
            0xffff_ffff
        };
        self.stky &= !LSEM;
    }

    pub fn pop_loop(&mut self) {
        if self.lstkp == 0 {
            return; // underflow
        }
        self.lstkp -= 1;
        self.lcntr = self.curlcntr;
        self.lastack[self.lstkp as usize] = self.laddr_pack();
        if self.lstkp > 0 {
            self.curlcntr = self.lcstack[(self.lstkp - 1) as usize];
            let packed = self.lastack[(self.lstkp - 1) as usize];
            self.laddr_unpack(packed);
        } else {
            self.curlcntr = 0xffff_ffff;
            self.laddr_unpack(0xffff_ffff);
            self.stky |= LSEM;
        }
    }

    // --- Status stack (push/pop sts, interrupts) ---
    pub fn push_status(&mut self) {
        if self.status_stkp < 4 {
            self.status_stkp += 1;
            self.status_stack[self.status_stkp as usize] = (self.mode1, self.astat);
        }
    }
    pub fn pop_status(&mut self) {
        if self.status_stkp > 0 {
            let (m, a) = self.status_stack[self.status_stkp as usize];
            self.mode1 = m;
            self.astat = a;
            self.status_stkp -= 1;
        }
    }
}
