use super::defs::I960Cpu;
use crate::bus::Bus;

impl I960Cpu {
    /// Calculate Effective Address (EA) based on addressing modes
    pub fn get_ea<B: Bus>(&mut self, bus: &mut B, opcode: u32) -> u32 {
        let abase = ((opcode >> 14) & 0x1f) as usize;

        // MEMA Format: Bit 12 is 0
        if (opcode & 0x00001000) == 0 {
            let offset = opcode & 0x1fff;
            if (opcode & 0x2000) == 0 {
                offset // Absolute
            } else {
                self.r[abase].wrapping_add(offset) // Register Indirect + Offset
            }
        }
        // MEMB Format: Bit 12 is 1
        else {
            let index = (opcode & 0x1f) as usize;
            let scale = (opcode >> 7) & 0x7;
            let mode = (opcode >> 10) & 0xf;

            match mode {
                0x4 => self.r[abase], // (abase)
                0x5 => {
                    // IP + disp + 8
                    let disp = bus.read_u32(self.ip);
                    self.ip = self.ip.wrapping_add(4);
                    disp.wrapping_add(self.ip) // Note: IP is already advanced
                }
                0x6 => self.r[index] << scale, // (index)*scale (New implementation)
                0x7 => self.r[abase].wrapping_add(self.r[index] << scale), // (abase) + (index)*scale
                0xc => {
                    // displacement (32-bit)
                    let disp = bus.read_u32(self.ip);
                    self.ip = self.ip.wrapping_add(4);
                    disp
                }
                0xd => {
                    // (abase) + displacement
                    let disp = bus.read_u32(self.ip);
                    self.ip = self.ip.wrapping_add(4);
                    disp.wrapping_add(self.r[abase])
                }
                0xe => {
                    // (index)*scale + displacement
                    let disp = bus.read_u32(self.ip);
                    self.ip = self.ip.wrapping_add(4);
                    disp.wrapping_add(self.r[index] << scale)
                }
                0xf => {
                    // (abase) + (index)*scale + displacement
                    let disp = bus.read_u32(self.ip);
                    self.ip = self.ip.wrapping_add(4);
                    disp.wrapping_add(self.r[abase])
                        .wrapping_add(self.r[index] << scale)
                }
                _ => panic!("I960: Unhandled MEMB mode {:x} at {:08x}", mode, self.pip),
            }
        }
    }

    /// Helper to get the first register operand or literal (RI)
    pub fn get_1_ri(&self, opcode: u32) -> u32 {
        if (opcode & 0x800) == 0 {
            self.r[(opcode & 0x1f) as usize]
        } else {
            opcode & 0x1f // Literal
        }
    }

    /// Helper to get the second register operand or literal (RI)
    pub fn get_2_ri(&self, opcode: u32) -> u32 {
        if (opcode & 0x1000) == 0 {
            self.r[((opcode >> 14) & 0x1f) as usize]
        } else {
            (opcode >> 14) & 0x1f // Literal
        }
    }

    /// Helper to set a register value
    pub fn set_ri(&mut self, opcode: u32, val: u32) {
        // If bit 13 (0x2000) is set in REG format, it's a literal destination (illegal)
        // But for valid instructions, destination is bits 19-23
        self.r[((opcode >> 19) & 0x1f) as usize] = val;
    }

    /// COBR src1: register or literal from bits 23-19, selected by bit 13.
    /// COBR packs its operands differently from REG format, so the `_ri`
    /// helpers must not be used for compare-and-branch instructions.
    pub fn get_1_ci(&self, opcode: u32) -> u32 {
        if (opcode & 0x2000) == 0 {
            self.r[((opcode >> 19) & 0x1f) as usize]
        } else {
            (opcode >> 19) & 0x1f // Literal
        }
    }

    /// COBR src2: always a register, from bits 18-14.
    pub fn get_2_ci(&self, opcode: u32) -> u32 {
        self.r[((opcode >> 14) & 0x1f) as usize]
    }

    /// Decode displacement for COBR/CTRL format
    pub fn get_disp(&self, opcode: u32) -> u32 {
        let val = opcode & 0x00FFFFFF; // 24 bits
        if val & 0x00800000 != 0 {
            (val | 0xFF000000).wrapping_sub(4)
        } else {
            val.wrapping_sub(4)
        }
    }

    /// Decode short displacement
    pub fn get_disp_s(&self, opcode: u32) -> u32 {
        let val = opcode & 0x1FFF; // 13 bits
        if val & 0x1000 != 0 {
            (val | 0xFFFFE000).wrapping_sub(4)
        } else {
            val.wrapping_sub(4)
        }
    }

    /// Unsigned comparison update of AC register
    pub fn cmp_u(&mut self, v1: u32, v2: u32) {
        self.ac &= !7; // Clear condition codes
        if v1 < v2 {
            self.ac |= 4;
        } else if v1 == v2 {
            self.ac |= 2;
        } else {
            self.ac |= 1;
        }
    }

    /// Signed comparison update of AC register
    pub fn cmp_s(&mut self, v1: u32, v2: u32) {
        self.ac &= !7;
        let i1 = v1 as i32;
        let i2 = v2 as i32;
        if i1 < i2 {
            self.ac |= 4;
        } else if i1 == i2 {
            self.ac |= 2;
        } else {
            self.ac |= 1;
        }
    }

    /// Conditional comparison: used by concmpo/concmpi, which only record
    /// "less-or-equal" vs "greater" and never set the less-than bit.
    pub fn concmp_u(&mut self, v1: u32, v2: u32) {
        self.ac &= !7;
        if v1 <= v2 {
            self.ac |= 2;
        } else {
            self.ac |= 1;
        }
    }

    pub fn concmp_s(&mut self, v1: u32, v2: u32) {
        self.ac &= !7;
        if (v1 as i32) <= (v2 as i32) {
            self.ac |= 2;
        } else {
            self.ac |= 1;
        }
    }

    /// Branch on Condition Code (Bxx) logic
    pub fn bxx(&mut self, opcode: u32, mask: u32) {
        if (self.ac & mask) != 0 {
            self.ip = self.ip.wrapping_add(self.get_disp(opcode));
            self.ip &= !3; // Align
        }
    }
}
