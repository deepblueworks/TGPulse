//! Program and data memory.
//!
//! Program memory is the DSP's own 4096-word RAM, filled by the host upload.
//! Data space is external and belongs to the board: buffer RAM shared with the i960 at 0x400000 with a
//! 0x3f8000 mirror, and the coprocessor data ROM at 0x800000.

use crate::state::Mb86235;
use crate::Mb86235Bus;

impl Mb86235 {
    /// Fetches the 64-bit instruction at `pc`.
    #[inline]
    pub fn fetch(&self, pc: u32) -> u64 {
        self.program
            .get((pc as usize) & (crate::state::PROGRAM_WORDS - 1))
            .copied()
            .unwrap_or(0)
    }

    #[inline]
    pub fn read_data<B: Mb86235Bus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        bus.data_read(addr)
    }

    #[inline]
    pub fn write_data<B: Mb86235Bus>(&mut self, bus: &mut B, addr: u32, data: u32) {
        bus.data_write(addr, data);
    }
}

impl Mb86235 {
    /// A-bus read: internal RAM A below 0x400, otherwise external memory
    /// offset by the EB base register.
    pub fn read_abus<B: Mb86235Bus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        if (addr & 0x3fff) >= 0x400 {
            bus.data_read((addr & 0x3fff) + (self.eb & 0xffc000))
        } else {
            self.dataa[(addr & 0x3ff) as usize]
        }
    }

    pub fn write_abus<B: Mb86235Bus>(&mut self, bus: &mut B, addr: u32, data: u32) {
        if (addr & 0x3fff) >= 0x400 {
            bus.data_write((addr & 0x3fff) + (self.eb & 0xffc000), data);
        } else {
            self.dataa[(addr & 0x3ff) as usize] = data;
        }
    }

    /// B-bus read/write: internal RAM B only.
    pub fn read_bbus(&mut self, addr: u32) -> u32 {
        self.datab[(addr & 0x3ff) as usize]
    }

    pub fn write_bbus(&mut self, addr: u32, data: u32) {
        self.datab[(addr & 0x3ff) as usize] = data;
    }

    /// Reads one of the 64 transfer-slot registers.
    pub fn get_transfer_reg<B: Mb86235Bus>(&mut self, bus: &mut B, which: u8) -> u32 {
        let n = (which & 7) as usize;
        match which >> 3 {
            0 => self.ma[n],
            1 => self.aa[n],
            2 => match n {
                0 => self.eb,
                1 => self.eb >> 14,
                2 => self.eb & 0x3fff,
                3 => self.eo,
                4 => self.sp,
                5 => self.st,
                6 => self.mod_,
                _ => self.lpc,
            },
            3 => self.ar[n],
            4 => self.mb[n],
            5 => self.ab[n],
            6 => match n {
                0 => self.pr[self.prp as usize],
                // FI: popping an empty input FIFO stalls the instruction so it
                // can be retried once the i960 has supplied a word.
                1 => match bus.fifo_in_pop() {
                    Some(v) => v,
                    None => {
                        self.stalled = true;
                        0
                    }
                },
                4 => self.pdr,
                5 => self.ddr,
                6 => self.prp,
                _ => self.pwp,
            },
            _ => 0,
        }
    }

    /// Writes one of the transfer-slot registers.
    pub fn set_transfer_reg<B: Mb86235Bus>(&mut self, bus: &mut B, which: u8, value: u32) {
        let n = (which & 7) as usize;
        match which >> 3 {
            0 => self.ma[n] = value,
            1 => self.aa[n] = value,
            2 => match n {
                0 => self.eb = value,
                1 => self.eb = (self.eb & 0x3fff) | (value << 14),
                2 => self.eb = (self.eb & 0xffc000) | (value << 14),
                3 => self.eo = value,
                4 => self.sp = value,
                5 => self.st = value,
                6 => self.mod_ = value,
                _ => self.lpc = value,
            },
            3 => self.ar[n] = value & 0x3fff,
            4 => self.mb[n] = value,
            5 => self.ab[n] = value,
            6 => match n {
                0 => {
                    self.pr[self.pwp as usize] = value;
                    if !self.stalled {
                        self.pwp = (self.pwp + 1) % 24;
                    }
                }
                // FO0/FO1 both feed the one output FIFO.
                2 | 3 => bus.fifo_out_push(value),
                4 => self.pdr = value,
                5 => self.ddr = value,
                6 => {
                    if value < 24 {
                        self.prp = value;
                    }
                }
                7 if value < 24 => {
                    self.pwp = value;
                }
                _ => {}
            },
            _ => {}
        }
    }
}
