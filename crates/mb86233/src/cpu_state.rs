use crate::alu::{get_exp, get_mant, set_exp, set_mant, AluState};
use crate::memory::Mb86233Bus;
use crate::types::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Mb86233 {
    // Registers
    pub pc: u16,
    pub ppc: u16,
    pub st: u32,
    pub sp: u16,

    // Main Register Bank
    pub a: u32,
    pub b: u32,
    pub d: u32,
    pub p: u32,

    // Control / Index Registers
    pub r: u8,
    pub rpc: u8,
    pub c0: u8,
    pub c1: u8,
    pub b0: u16,
    pub b1: u16,
    pub x0: u16,
    pub x1: u16,
    pub i0: u16,
    pub i1: u16,
    pub sft: u8,
    pub vsm: u8,
    pub vsmr: u16,
    pub mask: u16,
    pub m: u16,

    // Program Counter Stack
    pub pcs: [u16; 4],

    // ALU Internal State
    pub alu: AluState,

    // Execution State
    pub icount: i32,
    pub stall: bool,

    /// Coverage instrumentation. The core has a number of places where an
    /// unrecognised encoding falls through and does nothing at all; that is
    /// invisible at the time and only shows up much later as geometry that has
    /// quietly drifted. Counting them makes "the TGP is missing an opcode" a
    /// question with an answer instead of a suspicion.
    #[serde(skip)]
    pub cov: Coverage,
}

#[derive(Clone)]
pub struct Coverage {
    /// Instruction slots spent re-trying an access that stalled (an empty FIFO
    /// port). These do no work: the PC rewinds and the slot is burned.
    pub stall_retries: u64,
    /// Groups the decoder has no arm for at all.
    pub group_unknown: [u64; 64],
    /// ALU operations `alu_pre` does not implement.
    pub alu_unknown: [u64; 32],
    /// Internal registers `read_reg`/`write_reg` do not implement.
    pub read_reg_unknown: [u64; 64],
    pub write_reg_unknown: [u64; 64],
}

impl Default for Coverage {
    fn default() -> Self {
        Self {
            stall_retries: 0,
            group_unknown: [0; 64],
            alu_unknown: [0; 32],
            read_reg_unknown: [0; 64],
            write_reg_unknown: [0; 64],
        }
    }
}

impl Coverage {
    /// Total number of operations the core decoded but did not act on.
    pub fn unsupported_total(&self) -> u64 {
        self.group_unknown.iter().sum::<u64>()
            + self.alu_unknown.iter().sum::<u64>()
            + self.read_reg_unknown.iter().sum::<u64>()
            + self.write_reg_unknown.iter().sum::<u64>()
    }
}

impl Default for Mb86233 {
    fn default() -> Self {
        Self::new()
    }
}

impl Mb86233 {
    pub fn new() -> Self {
        let mut cpu = Self {
            pc: 0,
            ppc: 0,
            st: 0,
            sp: 0,
            a: 0,
            b: 0,
            d: 0,
            p: 0,
            r: 1,
            rpc: 1,
            c0: 1,
            c1: 1,
            b0: 0,
            b1: 0,
            x0: 0,
            x1: 0,
            i0: 0,
            i1: 0,
            sft: 0,
            vsm: 0,
            vsmr: 7,
            mask: 0,
            m: 1,
            pcs: [0; 4],
            alu: AluState::new(),
            icount: 0,
            stall: false,
            cov: Coverage::default(),
        };
        cpu.reset();
        cpu
    }

    pub fn reset(&mut self) {
        self.pc = 0;
        self.ppc = 0;
        // Default flags: ZRC|ZRD|ZX0|ZX1|ZX2|ZC0|ZC1
        self.st = F_ZRC | F_ZRD | F_ZX0 | F_ZX1 | F_ZX2 | F_ZC0 | F_ZC1;
        self.sp = 0;

        self.a = 0;
        self.b = 0;
        self.d = 0;
        self.p = 0;
        self.r = 1;
        self.rpc = 1;
        self.c0 = 1;
        self.c1 = 1;
        self.b0 = 0;
        self.b1 = 0;
        self.x0 = 0;
        self.x1 = 0;
        self.i0 = 0;
        self.i1 = 0;
        self.sft = 0;
        self.vsm = 0;
        self.vsmr = 7;
        self.mask = 0;
        self.m = 1;

        self.alu = AluState::new();
        self.pcs = [0; 4];
        self.stall = false;
    }

    /// Push current PC to the internal hardware stack
    pub fn pcs_push(&mut self) {
        for i in (1..4).rev() {
            self.pcs[i] = self.pcs[i - 1];
        }
        self.pcs[0] = self.pc;
    }

    /// Pop PC from the internal hardware stack
    pub fn pcs_pop(&mut self) {
        self.pc = self.pcs[0];
        for i in 0..3 {
            self.pcs[i] = self.pcs[i + 1];
        }
    }

    /// Handles reading from the register file or memory-mapped internal registers.
    /// Corresponds to `mb86233_device::read_reg`
    pub fn read_reg(&mut self, bus: &mut impl Mb86233Bus, r: u32) -> u32 {
        let r = r & 0x3f;

        // Register File Access (0x20 - 0x2F)
        if (0x20..0x30).contains(&r) {
            let v = bus.read_rf(r & 0x1f);
            // A FIFO port with nothing to give asks us to retry the instruction.
            if bus.take_stall() {
                self.stall = true;
            }
            return v;
        }

        match r {
            0x00 => self.b0 as u32,
            0x01 => self.b1 as u32,
            0x02 => self.x0 as u32,
            0x03 => self.x1 as u32,

            0x0C => self.c0 as u32,
            0x0D => self.c1 as u32,

            0x10 => self.a,
            0x11 => get_exp(self.a),
            0x12 => get_mant(self.a),
            0x13 => self.b,
            0x14 => get_exp(self.b),
            0x15 => get_mant(self.b),
            0x19 => self.d,
            0x1A => get_exp(self.d),
            0x1B => get_mant(self.d),
            0x1C => self.p,
            0x1D => get_exp(self.p),
            0x1E => get_mant(self.p),
            0x1F => self.sft as u32,

            0x34 => self.rpc as u32,

            _ => {
                self.cov.read_reg_unknown[r as usize] += 1;
                0
            }
        }
    }

    /// Handles writing to the register file or memory-mapped internal registers.
    /// Corresponds to `mb86233_device::write_reg`
    pub fn write_reg(&mut self, bus: &mut impl Mb86233Bus, r: u32, v: u32) {
        let r = r & 0x3f;

        // Register File Access (0x20 - 0x2F)
        if (0x20..0x30).contains(&r) {
            bus.write_rf(r & 0x1f, v);
            return;
        }

        match r {
            0x00 => self.b0 = v as u16,
            0x01 => self.b1 = v as u16,
            0x02 => self.x0 = v as u16,
            0x03 => self.x1 = v as u16,

            0x05 => self.i0 = v as u16,
            0x06 => self.i1 = v as u16,

            0x08 => self.sp = v as u16,

            0x0A => {
                self.vsm = (v & 7) as u8;
                // vsmr = (8 << vsm) - 1;
                self.vsmr = (8u16.wrapping_shl(self.vsm as u32)).wrapping_sub(1);
            }

            0x0C => {
                self.c0 = v as u8;
                if self.c0 == 1 {
                    self.st |= F_ZC0;
                } else {
                    self.st &= !F_ZC0;
                }
            }

            0x0D => {
                self.c1 = v as u8;
                if self.c1 == 1 {
                    self.st |= F_ZC1;
                } else {
                    self.st &= !F_ZC1;
                }
            }

            0x0F => { /* No-op based on source */ }

            0x10 => self.a = v,
            0x11 => self.a = set_exp(self.a, v),
            0x12 => self.a = set_mant(self.a, v),
            0x13 => self.b = v,
            0x14 => self.b = set_exp(self.b, v),
            0x15 => self.b = set_mant(self.b, v),

            0x19 => self.d = v,
            0x1A => self.d = set_exp(self.d, v),
            0x1B => self.d = set_mant(self.d, v),
            0x1C => self.p = v,
            0x1D => self.p = set_exp(self.p, v),
            0x1E => self.p = set_mant(self.p, v),
            0x1F => self.sft = v as u8,

            0x34 => self.rpc = v as u8,
            0x3C => self.mask = v as u16,

            _ => {
                self.cov.write_reg_unknown[r as usize] += 1;
            }
        }
    }
}
