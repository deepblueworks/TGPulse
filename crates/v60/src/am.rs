//! V60 addressing-mode address decoder.
//!
//! Given the operand at `modadd`, decodes the effective address into `amout`
//! and sets `amflag` (false = memory address, true = a register index). Returns
//! the operand's byte length so the opcode can advance PC. Dispatch mirrors
//! The reference exactly: `modval >> 5` picks one of eight groups, group 6/7 sub-dispatch
//! on further bits. `moddim` selects the operand size, and for indexed modes the
//! index scale is `1 << moddim` (x1/x2/x4/x8).

use crate::bus::Bus;
use crate::cpu::{PC, V60};

impl V60 {
    #[inline]
    fn reg_n(&self, n: u8) -> u32 {
        self.reg[(n & 0x1f) as usize]
    }
    #[inline]
    fn scale(&self) -> u32 {
        1u32 << self.moddim
    }

    /// Decodes the operand address at `self.modadd`. Sets `amout`/`amflag`,
    /// returns the operand length in bytes.
    pub(crate) fn read_am_address<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.modval = bus.read_u8(self.modadd);
        self.am_value = false;
        let a = self.modadd;
        let n = self.modval;
        let base = self.reg_n(n);
        let sc = self.scale();

        match (self.modm, (self.modval >> 5) & 7) {
            // --- modm == 0 ---
            (false, 0) => {
                // Displacement8
                self.amflag = false;
                self.amout = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                2
            }
            (false, 1) => {
                // Displacement16
                self.amflag = false;
                self.amout = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                3
            }
            (false, 2) => {
                // Displacement32
                self.amflag = false;
                self.amout = base.wrapping_add(bus.read_u32(a + 1));
                5
            }
            (false, 3) => {
                // RegisterIndirect
                self.amflag = false;
                self.amout = base;
                1
            }
            (false, 4) => {
                // DisplacementIndirect8
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                self.amout = bus.read_u32(p);
                2
            }
            (false, 5) => {
                // DisplacementIndirect16
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                self.amout = bus.read_u32(p);
                3
            }
            (false, 6) => {
                // DisplacementIndirect32
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u32(a + 1));
                self.amout = bus.read_u32(p);
                5
            }
            (false, 7) => self.am2_group7(bus),

            // --- modm == 1 ---
            (true, 0) => {
                // DoubleDisplacement8
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                self.amout = bus
                    .read_u32(p)
                    .wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                3
            }
            (true, 1) => {
                // DoubleDisplacement16
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                self.amout = bus
                    .read_u32(p)
                    .wrapping_add(bus.read_u16(a + 3) as i16 as u32);
                5
            }
            (true, 2) => {
                // DoubleDisplacement32
                self.amflag = false;
                let p = base.wrapping_add(bus.read_u32(a + 1));
                self.amout = bus.read_u32(p).wrapping_add(bus.read_u32(a + 5));
                9
            }
            (true, 3) => {
                // Register
                self.amflag = true;
                self.amout = (n & 0x1f) as u32;
                1
            }
            (true, 4) => {
                // Autoincrement
                self.amflag = false;
                self.amout = base;
                let r = (n & 0x1f) as usize;
                self.reg[r] = self.reg[r].wrapping_add(sc);
                1
            }
            (true, 5) => {
                // Autodecrement
                self.amflag = false;
                let r = (n & 0x1f) as usize;
                self.reg[r] = self.reg[r].wrapping_sub(sc);
                self.amout = self.reg[r];
                1
            }
            (true, 6) => self.am2_group6(bus),
            (true, 7) => 0, // Error1
            _ => unreachable!(),
        }
    }

    /// Group 7 (modm=0): immediate / PC-relative / absolute modes, selected by
    /// the low 5 bits of the mode byte.
    fn am2_group7<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let pc = self.reg[PC];
        match self.modval & 0x1f {
            // 0x00-0x0f: immediate quick -- the value is the low nibble.
            0x00..=0x0f => {
                self.amflag = false;
                self.am_value = true;
                self.amout = (self.modval & 0xf) as u32;
                1
            }
            0x10 => {
                self.amflag = false;
                self.amout = pc.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                2
            }
            0x11 => {
                self.amflag = false;
                self.amout = pc.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                3
            }
            0x12 => {
                self.amflag = false;
                self.amout = pc.wrapping_add(bus.read_u32(a + 1));
                5
            }
            0x13 => {
                self.amflag = false;
                self.amout = bus.read_u32(a + 1);
                5
            } // DirectAddress
            0x14 => self.am1_immediate(bus), // Immediate (value)
            0x18 => {
                self.amflag = false;
                let d = bus.read_u8(a + 1) as i8 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                2
            }
            0x19 => {
                self.amflag = false;
                let d = bus.read_u16(a + 1) as i16 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                3
            }
            0x1a => {
                self.amflag = false;
                let d = bus.read_u32(a + 1);
                self.amout = bus.read_u32(pc.wrapping_add(d));
                5
            }
            0x1b => {
                self.amflag = false;
                let d = bus.read_u32(a + 1);
                self.amout = bus.read_u32(d);
                5
            } // DirectAddressDeferred
            0x1c => {
                // PCDoubleDisplacement8
                self.amflag = false;
                let d = bus.read_u8(a + 1) as i8 as u32;
                let e = bus.read_u8(a + 2) as i8 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d)).wrapping_add(e);
                3
            }
            0x1d => {
                self.amflag = false;
                let d = bus.read_u16(a + 1) as i16 as u32;
                let e = bus.read_u16(a + 3) as i16 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d)).wrapping_add(e);
                5
            }
            0x1e => {
                self.amflag = false;
                let d = bus.read_u32(a + 1);
                let e = bus.read_u32(a + 5);
                self.amout = bus.read_u32(pc.wrapping_add(d)).wrapping_add(e);
                9
            }
            _ => 0, // Error2 / reserved
        }
    }

    /// Group 6 (modm=1): register-indirect indexed and displacement indexed,
    /// with a second mode byte selecting the index register.
    fn am2_group6<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let m2 = bus.read_u8(a + 1);
        self.modval2 = m2;
        let idx = self.reg_n(self.modval) * self.scale();
        let base2 = self.reg_n(m2);
        self.amflag = false;
        match (m2 >> 5) & 7 {
            0 => {
                self.amout = base2
                    .wrapping_add(bus.read_u8(a + 2) as i8 as u32)
                    .wrapping_add(idx);
                3
            }
            1 => {
                self.amout = base2
                    .wrapping_add(bus.read_u16(a + 2) as i16 as u32)
                    .wrapping_add(idx);
                4
            }
            2 => {
                self.amout = base2.wrapping_add(bus.read_u32(a + 2)).wrapping_add(idx);
                6
            }
            3 => {
                self.amout = base2.wrapping_add(idx);
                2
            } // RegisterIndirectIndexed
            4 => {
                let p = base2.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p).wrapping_add(idx);
                3
            }
            5 => {
                let p = base2.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p).wrapping_add(idx);
                4
            }
            6 => {
                let p = base2.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p).wrapping_add(idx);
                6
            }
            7 => self.am2_group7a(bus),
            _ => unreachable!(),
        }
    }

    /// Group 7a (from group 6, index register 7): PC-relative and absolute
    /// indexed modes.
    fn am2_group7a<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let pc = self.reg[PC];
        let m2 = self.modval2;
        if m2 & 0x10 == 0 {
            return 0; // Error4
        }
        let idx = self.reg_n(self.modval) * self.scale();
        self.amflag = false;
        match m2 & 0xf {
            0 => {
                self.amout = pc
                    .wrapping_add(bus.read_u8(a + 2) as i8 as u32)
                    .wrapping_add(idx);
                3
            }
            1 => {
                self.amout = pc
                    .wrapping_add(bus.read_u16(a + 2) as i16 as u32)
                    .wrapping_add(idx);
                4
            }
            2 => {
                self.amout = pc.wrapping_add(bus.read_u32(a + 2)).wrapping_add(idx);
                6
            }
            3 => {
                self.amout = bus.read_u32(a + 2).wrapping_add(idx);
                6
            } // DirectAddressIndexed
            8 => {
                let p = pc.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p).wrapping_add(idx);
                3
            }
            9 => {
                let p = pc.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p).wrapping_add(idx);
                4
            }
            10 => {
                let p = pc.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p).wrapping_add(idx);
                6
            }
            11 => {
                let p = bus.read_u32(a + 2);
                self.amout = bus.read_u32(p).wrapping_add(idx);
                6
            } // DirectAddressDeferredIndexed
            _ => 0, // Error5
        }
    }

    /// Immediate value read (am1Immediate), used where an operand is a literal.
    fn am1_immediate<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        self.amflag = false;
        self.am_value = true;
        match self.moddim {
            0 => {
                self.amout = bus.read_u8(a + 1) as u32;
                2
            }
            1 => {
                self.amout = bus.read_u16(a + 1) as u32;
                3
            }
            _ => {
                self.amout = bus.read_u32(a + 1);
                5
            }
        }
    }

    /// BitReadAMAddress: the bit-address twin
    /// of `read_am_address`. Decodes the operand at `modadd` into a byte base
    /// address in `amout` plus a bit offset in `bamoffset`, and returns the
    /// operand length. Displacements become the bit offset instead of being
    /// added to the base; indexed forms take the index register's raw value
    /// as the bit offset (no `moddim` scaling). Register-direct and immediate
    /// forms have no bit-address meaning: a real part faults on them
    /// (bam2Error2/4/5/6), here they decode to length 0 like the am2 errors.
    pub(crate) fn bit_read_am_address<B: Bus>(&mut self, bus: &mut B) -> u32 {
        self.modval = bus.read_u8(self.modadd);
        self.am_value = false;
        self.amflag = false;
        let a = self.modadd;
        let n = self.modval;
        let base = self.reg_n(n);

        match (self.modm, (self.modval >> 5) & 7) {
            // --- modm == 0 ---
            (false, 0) => {
                // Displacement8: base register + bit displacement
                self.amout = base;
                self.bamoffset = bus.read_u8(a + 1) as i8 as u32;
                2
            }
            (false, 1) => {
                // Displacement16
                self.amout = base;
                self.bamoffset = bus.read_u16(a + 1) as i16 as u32;
                3
            }
            (false, 2) => {
                // Displacement32
                self.amout = base;
                self.bamoffset = bus.read_u32(a + 1);
                5
            }
            (false, 3) => {
                // RegisterIndirect
                self.amout = base;
                self.bamoffset = 0;
                1
            }
            (false, 4) => {
                // DisplacementIndirect8
                let p = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = 0;
                2
            }
            (false, 5) => {
                // DisplacementIndirect16
                let p = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = 0;
                3
            }
            (false, 6) => {
                // DisplacementIndirect32
                let p = base.wrapping_add(bus.read_u32(a + 1));
                self.amout = bus.read_u32(p);
                self.bamoffset = 0;
                5
            }
            (false, 7) => self.bam2_group7(bus),

            // --- modm == 1 ---
            (true, 0) => {
                // DoubleDisplacement8
                let p = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = bus.read_u8(a + 2) as i8 as u32;
                3
            }
            (true, 1) => {
                // DoubleDisplacement16: the reference reads the bit offset as int8
                let p = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = bus.read_u8(a + 3) as i8 as u32;
                5
            }
            (true, 2) => {
                // DoubleDisplacement32
                let p = base.wrapping_add(bus.read_u32(a + 1));
                self.amout = bus.read_u32(p);
                self.bamoffset = bus.read_u32(a + 5);
                9
            }
            (true, 3) => 0, // bam2Error6: register-direct has no bit address
            (true, 4) => {
                // Autoincrement: dim 10 steps 1 byte, dim 11 steps 4
                self.amout = base;
                self.bamoffset = 0;
                let r = (n & 0x1f) as usize;
                match self.moddim {
                    10 => self.reg[r] = self.reg[r].wrapping_add(1),
                    11 => self.reg[r] = self.reg[r].wrapping_add(4),
                    _ => return 0, // the reference fatal-errors on any other dim
                }
                1
            }
            (true, 5) => {
                // Autodecrement
                self.bamoffset = 0;
                let r = (n & 0x1f) as usize;
                match self.moddim {
                    10 => self.reg[r] = self.reg[r].wrapping_sub(1),
                    11 => self.reg[r] = self.reg[r].wrapping_sub(4),
                    _ => return 0,
                }
                self.amout = self.reg[r];
                1
            }
            (true, 6) => self.bam2_group6(bus),
            (true, 7) => 0, // Error1
            _ => unreachable!(),
        }
    }

    /// bam2 group 7 (modm=0): PC-relative / absolute bit-address modes,
    /// selected by the low 5 bits of the mode byte. The immediate forms
    /// (0x00-0x0f, 0x14) are bam2Error6 in the reference.
    fn bam2_group7<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let pc = self.reg[PC];
        match self.modval & 0x1f {
            0x10 => {
                self.amout = pc;
                self.bamoffset = bus.read_u8(a + 1) as i8 as u32;
                2
            }
            0x11 => {
                self.amout = pc;
                self.bamoffset = bus.read_u16(a + 1) as i16 as u32;
                3
            }
            0x12 => {
                self.amout = pc;
                self.bamoffset = bus.read_u32(a + 1);
                5
            }
            0x13 => {
                self.amout = bus.read_u32(a + 1);
                self.bamoffset = 0;
                5
            } // DirectAddress
            0x18 => {
                let d = bus.read_u8(a + 1) as i8 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = 0;
                2
            }
            0x19 => {
                let d = bus.read_u16(a + 1) as i16 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = 0;
                3
            }
            0x1a => {
                let d = bus.read_u32(a + 1);
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = 0;
                5
            }
            0x1b => {
                let d = bus.read_u32(a + 1);
                self.amout = bus.read_u32(d);
                self.bamoffset = 0;
                5
            } // DirectAddressDeferred
            0x1c => {
                // PCDoubleDisplacement8
                let d = bus.read_u8(a + 1) as i8 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = bus.read_u8(a + 2) as i8 as u32;
                3
            }
            0x1d => {
                // PCDoubleDisplacement16: bit offset read as int8
                let d = bus.read_u16(a + 1) as i16 as u32;
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = bus.read_u8(a + 3) as i8 as u32;
                5
            }
            0x1e => {
                // PCDoubleDisplacement32
                let d = bus.read_u32(a + 1);
                self.amout = bus.read_u32(pc.wrapping_add(d));
                self.bamoffset = bus.read_u32(a + 5);
                9
            }
            _ => 0, // Error2 / Error6 / reserved
        }
    }

    /// bam2 group 6 (modm=1): indexed bit-address modes. The first mode
    /// byte's register is the index, whose raw value becomes the bit offset;
    /// the second byte selects the base form.
    fn bam2_group6<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let m2 = bus.read_u8(a + 1);
        self.modval2 = m2;
        let idx = self.reg_n(self.modval);
        let base2 = self.reg_n(m2);
        match (m2 >> 5) & 7 {
            0 => {
                self.amout = base2.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.bamoffset = idx;
                3
            }
            1 => {
                self.amout = base2.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.bamoffset = idx;
                4
            }
            2 => {
                self.amout = base2.wrapping_add(bus.read_u32(a + 2));
                self.bamoffset = idx;
                6
            }
            3 => {
                self.amout = base2;
                self.bamoffset = idx;
                2
            } // RegisterIndirectIndexed
            4 => {
                let p = base2.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                3
            }
            5 => {
                let p = base2.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                4
            }
            6 => {
                let p = base2.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                6
            }
            7 => self.bam2_group7a(bus),
            _ => unreachable!(),
        }
    }

    /// bam2 group 7a (from group 6): PC-relative and absolute indexed
    /// bit-address modes.
    fn bam2_group7a<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let a = self.modadd;
        let pc = self.reg[PC];
        let m2 = self.modval2;
        if m2 & 0x10 == 0 {
            return 0; // Error4
        }
        let idx = self.reg_n(self.modval);
        match m2 & 0xf {
            0 => {
                self.amout = pc.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.bamoffset = idx;
                3
            }
            1 => {
                self.amout = pc.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.bamoffset = idx;
                4
            }
            2 => {
                self.amout = pc.wrapping_add(bus.read_u32(a + 2));
                self.bamoffset = idx;
                6
            }
            3 => {
                self.amout = bus.read_u32(a + 2);
                self.bamoffset = idx;
                6
            } // DirectAddressIndexed
            8 => {
                let p = pc.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                3
            }
            9 => {
                let p = pc.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                4
            }
            10 => {
                let p = pc.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                6
            }
            11 => {
                let p = bus.read_u32(a + 2);
                self.amout = bus.read_u32(p);
                self.bamoffset = idx;
                6
            } // DirectAddressDeferredIndexed
            _ => 0, // Error5
        }
    }

    /// BitReadAM: the bit-field twin of
    /// `bit_read_am_address`, used by the EXTBF/INSBF source operand. Decodes
    /// the operand at `modadd`, reads the 32-bit value it addresses into
    /// `amout`, and leaves the bit index within that value in `bamoffset`.
    /// Unlike bam2, the plain displacement offsets are read UNSIGNED; only the
    /// address displacements of the indexed/double-displacement forms are
    /// signed. Error modes return None, which the caller
    /// reports as an unimplemented opcode.
    pub(crate) fn bit_read_am<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        self.modval = bus.read_u8(self.modadd);
        self.amflag = false;
        let a = self.modadd;
        let n = self.modval;
        let base = self.reg_n(n);

        Some(match (self.modm, (n >> 5) & 7) {
            // --- modm == 0 ---
            (false, 0) => {
                // Displacement8 (unsigned bit offset)
                let bit = bus.read_u8(a + 1) as u32;
                self.amout = bus.read_u32(base.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                2
            }
            (false, 1) => {
                // Displacement16
                let bit = bus.read_u16(a + 1) as u32;
                self.amout = bus.read_u32(base.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                3
            }
            (false, 2) => {
                // Displacement32
                let bit = bus.read_u32(a + 1);
                self.amout = bus.read_u32(base.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                5
            }
            (false, 3) => {
                // RegisterIndirect
                self.bamoffset = 0;
                self.amout = bus.read_u32(base);
                1
            }
            (false, 4) => {
                // DisplacementIndirect8
                self.bamoffset = 0;
                let p = base.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                2
            }
            (false, 5) => {
                // DisplacementIndirect16
                self.bamoffset = 0;
                let p = base.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                3
            }
            (false, 6) => {
                // DisplacementIndirect32
                self.bamoffset = 0;
                let p = base.wrapping_add(bus.read_u32(a + 1));
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                5
            }
            (false, 7) => self.bam1_group7(bus)?,

            // --- modm == 1 ---
            (true, 0) => {
                // DoubleDisplacement8
                let bit = bus.read_u8(a + 2) as u32;
                let t = bus.read_u8(a + 1) as i8 as u32;
                let p = bus.read_u32(base.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                3
            }
            (true, 1) => {
                // DoubleDisplacement16
                let bit = bus.read_u16(a + 3) as u32;
                let t = bus.read_u16(a + 1) as i16 as u32;
                let p = bus.read_u32(base.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                5
            }
            (true, 2) => {
                // DoubleDisplacement32
                let bit = bus.read_u32(a + 5);
                let t = bus.read_u32(a + 1);
                let p = bus.read_u32(base.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                9
            }
            (true, 3) => return None, // bam1Error6: register-direct has no bit address
            (true, 4) => {
                // Autoincrement: dim 10 steps 1 byte, dim 11 steps 4
                self.bamoffset = 0;
                self.amout = bus.read_u32(base);
                let r = (n & 0x1f) as usize;
                match self.moddim {
                    10 => self.reg[r] = self.reg[r].wrapping_add(1),
                    11 => self.reg[r] = self.reg[r].wrapping_add(4),
                    _ => return None,
                }
                1
            }
            (true, 5) => {
                // Autodecrement
                self.bamoffset = 0;
                let r = (n & 0x1f) as usize;
                match self.moddim {
                    10 => self.reg[r] = self.reg[r].wrapping_sub(1),
                    11 => self.reg[r] = self.reg[r].wrapping_sub(4),
                    _ => return None,
                }
                self.amout = bus.read_u32(self.reg[r]);
                1
            }
            (true, 6) => self.bam1_group6(bus)?,
            (true, 7) => return None, // bam1Error1
            _ => unreachable!(),
        })
    }

    /// bam1 group 7 (modm=0): PC-relative / absolute forms. Bit offsets are
    /// unsigned; the address displacements are signed.
    fn bam1_group7<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        let a = self.modadd;
        let pc = self.reg[PC];
        Some(match self.modval & 0x1f {
            0x10 => {
                let bit = bus.read_u8(a + 1) as u32;
                self.amout = bus.read_u32(pc.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                2
            }
            0x11 => {
                let bit = bus.read_u16(a + 1) as u32;
                self.amout = bus.read_u32(pc.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                3
            }
            0x12 => {
                let bit = bus.read_u32(a + 1);
                self.amout = bus.read_u32(pc.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                5
            }
            0x13 => {
                self.bamoffset = 0;
                let q = bus.read_u32(a + 1);
                self.amout = bus.read_u32(q);
                5
            } // DirectAddress
            0x18 => {
                self.bamoffset = 0;
                let p = pc.wrapping_add(bus.read_u8(a + 1) as i8 as u32);
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                2
            }
            0x19 => {
                self.bamoffset = 0;
                let p = pc.wrapping_add(bus.read_u16(a + 1) as i16 as u32);
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                3
            }
            0x1a => {
                self.bamoffset = 0;
                let p = pc.wrapping_add(bus.read_u32(a + 1));
                let q = bus.read_u32(p);
                self.amout = bus.read_u32(q);
                5
            }
            0x1b => {
                self.bamoffset = 0;
                let t = bus.read_u32(a + 1);
                let q = bus.read_u32(t);
                self.amout = bus.read_u32(q);
                5
            } // DirectAddressDeferred
            0x1c => {
                // PCDoubleDisplacement8
                let bit = bus.read_u8(a + 2) as u32;
                let t = bus.read_u8(a + 1) as i8 as u32;
                let p = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                3
            }
            0x1d => {
                // PCDoubleDisplacement16
                let bit = bus.read_u16(a + 3) as u32;
                let t = bus.read_u16(a + 1) as i16 as u32;
                let p = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                5
            }
            0x1e => {
                // PCDoubleDisplacement32
                let bit = bus.read_u32(a + 5);
                let t = bus.read_u32(a + 1);
                let p = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(bit / 8));
                self.bamoffset = bit & 7;
                9
            }
            _ => return None, // Error2 / Error6 / reserved
        })
    }

    /// bam1 group 6 (modm=1): indexed forms. The first mode byte's register is
    /// the index, whose raw value is the bit offset; the second byte selects
    /// the base form.
    fn bam1_group6<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        let a = self.modadd;
        let m2 = bus.read_u8(a + 1);
        self.modval2 = m2;
        let idx = self.reg_n(self.modval);
        let base2 = self.reg_n(m2);
        Some(match (m2 >> 5) & 7 {
            0 => {
                let p = base2.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                3
            }
            1 => {
                let p = base2.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                4
            }
            2 => {
                let p = base2.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            }
            3 => {
                self.amout = bus.read_u32(base2.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                2
            } // RegisterIndirectIndexed
            4 => {
                let t = bus.read_u8(a + 2) as i8 as u32;
                let q = bus.read_u32(base2.wrapping_add(t));
                self.amout = bus.read_u32(q.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                3
            }
            5 => {
                let t = bus.read_u16(a + 2) as i16 as u32;
                let q = bus.read_u32(base2.wrapping_add(t));
                self.amout = bus.read_u32(q.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                4
            }
            6 => {
                let t = bus.read_u32(a + 2);
                let p = bus.read_u32(base2.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            }
            7 => self.bam1_group7a(bus)?,
            _ => unreachable!(),
        })
    }

    /// bam1 group 7a (from group 6): PC-relative and absolute indexed forms.
    fn bam1_group7a<B: Bus>(&mut self, bus: &mut B) -> Option<u32> {
        let a = self.modadd;
        let pc = self.reg[PC];
        let m2 = self.modval2;
        if m2 & 0x10 == 0 {
            return None; // Error4
        }
        let idx = self.reg_n(self.modval);
        Some(match m2 & 0xf {
            0 => {
                let p = pc.wrapping_add(bus.read_u8(a + 2) as i8 as u32);
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                3
            }
            1 => {
                let p = pc.wrapping_add(bus.read_u16(a + 2) as i16 as u32);
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                4
            }
            2 => {
                let p = pc.wrapping_add(bus.read_u32(a + 2));
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            }
            3 => {
                let p = bus.read_u32(a + 2);
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            } // DirectAddressIndexed
            8 => {
                let t = bus.read_u8(a + 2) as i8 as u32;
                let q = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(q.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                3
            }
            9 => {
                let t = bus.read_u16(a + 2) as i16 as u32;
                let q = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(q.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                4
            }
            10 => {
                let t = bus.read_u32(a + 2);
                let p = bus.read_u32(pc.wrapping_add(t));
                self.amout = bus.read_u32(p.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            }
            11 => {
                let q0 = bus.read_u32(a + 2);
                let q = bus.read_u32(q0);
                self.amout = bus.read_u32(q.wrapping_add(idx / 8));
                self.bamoffset = idx & 7;
                6
            } // DirectAddressDeferredIndexed
            _ => return None, // Error5
        })
    }
}
