//! SHARC memory access: internal SRAM (PM 48-bit, DM 32-bit) plus the external
//! program/data space reached through `SharcBus`, and the external DMA the i960
//! uses to upload the microcode.

use crate::state::Sharc;
use crate::SharcBus;

/// First internal RAM word; all internal addresses are offset from here.
const INTERNAL_BASE: u32 = 0x20000;
/// One past the last internal address (the 21062's two banks plus mirrors).
const INTERNAL_END: u32 = 0x40000;
/// Words per bank on the 2Mbit part.
const BANK_WORDS: u32 = 0x8000;

impl Sharc {
    /// Maps a data-memory address onto the internal SRAM.
    ///
    /// The reference puts block 0 at 0x20000-0x27fff and mirrors block 1
    /// across 0x28000-0x3ffff (mirror mask 0x18000). Missing the second bank
    /// silently sends every store above 0x28000 out to the external bus, which
    /// is where Virtua Striker's geometry results were disappearing to.
    #[inline]
    fn internal(addr: u32) -> Option<usize> {
        if !(INTERNAL_BASE..INTERNAL_END).contains(&addr) {
            return None;
        }
        let off = addr - INTERNAL_BASE;
        Some(if off < BANK_WORDS {
            off as usize
        } else {
            (BANK_WORDS + (addr & (BANK_WORDS - 1))) as usize
        })
    }

    /// Reads a 48-bit program-memory word.
    pub fn pm_read48<B: SharcBus>(&mut self, bus: &mut B, addr: u32) -> u64 {
        match Self::internal(addr) {
            Some(i) => self.pm[i] & 0xffff_ffff_ffff,
            None => bus.pm_ext_read(addr),
        }
    }

    /// Writes a 48-bit program-memory word.
    pub fn pm_write48<B: SharcBus>(&mut self, bus: &mut B, addr: u32, data: u64) {
        match Self::internal(addr) {
            Some(i) => self.pm[i] = data & 0xffff_ffff_ffff,
            None => bus.pm_ext_write(addr, data),
        }
    }

    /// Reads a 32-bit program-memory word (the upper 32 of the 48-bit slot).
    pub fn pm_read32<B: SharcBus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        match Self::internal(addr) {
            Some(i) => (self.pm[i] >> 16) as u32,
            None => bus.pm_ext_read(addr) as u32,
        }
    }

    /// Writes a 32-bit program-memory word into the upper 32 of the slot.
    pub fn pm_write32<B: SharcBus>(&mut self, bus: &mut B, addr: u32, data: u32) {
        match Self::internal(addr) {
            Some(i) => self.pm[i] = (self.pm[i] & 0xffff) | ((data as u64) << 16),
            None => bus.pm_ext_write(addr, (data as u64) << 16),
        }
    }

    /// Reads a 32-bit data-memory word.
    pub fn dm_read32<B: SharcBus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        if addr < 0x100 {
            return self.iop_read(addr);
        }
        match Self::internal(addr) {
            Some(i) => self.dm[i],
            None => bus.dm_ext_read(addr),
        }
    }

    /// Writes a 32-bit data-memory word.
    pub fn dm_write32<B: SharcBus>(&mut self, bus: &mut B, addr: u32, data: u32) {
        if addr < 0x100 {
            self.iop_write(bus, addr, data);
            return;
        }
        match Self::internal(addr) {
            Some(i) => self.dm[i] = data,
            None => bus.dm_ext_write(addr, data),
        }
    }

    /// The I/O processor register file (data space 0x000-0x0ff). Offsets are
    /// The reference: the DMA channels are *not* a uniform array --
    /// channel 6's control is at 0x1c with its registers at 0x40-0x47, and
    /// channel 7's at 0x1d / 0x48-0x4f.
    fn iop_read(&self, addr: u32) -> u32 {
        match addr {
            0x00 => self.syscon,
            0x37 => self.dma_status,
            0x40 => self.dma[6].int_index,
            0x41 => self.dma[6].int_modifier,
            0x42 => self.dma[6].int_count,
            0x43 => self.dma[6].chain_ptr,
            0x44 => self.dma[6].gen_purpose,
            0x45 => self.dma[6].ext_index,
            0x46 => self.dma[6].ext_modifier,
            0x47 => self.dma[6].ext_count,
            0x48 => self.dma[7].int_index,
            0x49 => self.dma[7].int_modifier,
            0x4a => self.dma[7].int_count,
            0x4b => self.dma[7].chain_ptr,
            0x4c => self.dma[7].gen_purpose,
            0x4d => self.dma[7].ext_index,
            0x4e => self.dma[7].ext_modifier,
            0x4f => self.dma[7].ext_count,
            _ => 0,
        }
    }

    fn iop_write<B: SharcBus>(&mut self, bus: &mut B, addr: u32, data: u32) {
        match addr {
            0x00 => self.syscon = data,
            0x04 => {
                // External-port DMA buffer: the packed microcode path.
                let shift = self.extdma_shift as u32;
                self.external_dma_write(bus, shift, data);
                self.extdma_shift = (self.extdma_shift + 1) % 3;
            }
            0x1c => {
                self.dma[6].control = data;
                if data & 1 != 0 {
                    self.dma_exec(bus, 6);
                }
            }
            0x1d => {
                self.dma[7].control = data;
                if data & 1 != 0 {
                    self.dma_exec(bus, 7);
                }
            }
            0x40 => self.dma[6].int_index = data,
            0x41 => self.dma[6].int_modifier = data,
            0x42 => self.dma[6].int_count = data,
            0x43 => self.dma[6].chain_ptr = data,
            0x44 => self.dma[6].gen_purpose = data,
            0x45 => self.dma[6].ext_index = data,
            0x46 => self.dma[6].ext_modifier = data,
            0x47 => self.dma[6].ext_count = data,
            0x48 => self.dma[7].int_index = data,
            0x49 => self.dma[7].int_modifier = data,
            0x4a => self.dma[7].int_count = data,
            0x4b => self.dma[7].chain_ptr = data,
            0x4c => self.dma[7].gen_purpose = data,
            0x4d => self.dma[7].ext_index = data,
            0x4e => self.dma[7].ext_modifier = data,
            0x4f => self.dma[7].ext_count = data,
            _ => {}
        }
    }

    /// Runs a DMA channel to completion and raises its completion interrupt.
    ///
    /// The reference schedules the transfer on a timer; running it synchronously is
    /// equivalent for this interpreter -- what the microcode observes is the
    /// data in place and IRPTL bit (channel + 10) set.
    pub fn dma_exec<B: SharcBus>(&mut self, bus: &mut B, channel: usize) {
        let control = self.dma[channel].control;
        let chen = (control >> 1) & 1;
        let tran = (control >> 2) & 1;
        let dtype = (control >> 5) & 1;
        let mut pmode = (control >> 6) & 3;
        if chen != 0 {
            // Chained DMA is not used by the Model 2 microcode.
            return;
        }
        let (src, src_modifier, src_count, dst, dst_modifier) = if tran != 0 {
            // Transmit to external
            (
                (self.dma[channel].int_index & 0x1ffff) | 0x20000,
                self.dma[channel].int_modifier,
                self.dma[channel].int_count,
                self.dma[channel].ext_index,
                self.dma[channel].ext_modifier,
            )
        } else {
            // Receive from external
            (
                self.dma[channel].ext_index,
                self.dma[channel].ext_modifier,
                self.dma[channel].ext_count,
                (self.dma[channel].int_index & 0x1ffff) | 0x20000,
                self.dma[channel].int_modifier,
            )
        };
        if dtype != 0 {
            pmode = 2; // 8/48 packing
        }

        let (mut s, mut d) = (src, dst);
        match pmode {
            0 => {
                for _ in 0..src_count {
                    let v = self.dm_read32(bus, s);
                    self.dm_write32(bus, d, v);
                    s = s.wrapping_add(src_modifier);
                    d = d.wrapping_add(dst_modifier);
                }
            }
            1 => {
                // 16/32 packing
                for _ in 0..(src_count / 2) {
                    let hi = self.dm_read32(bus, s) & 0xffff;
                    let lo = self.dm_read32(bus, s.wrapping_add(1)) & 0xffff;
                    self.dm_write32(bus, d, (hi << 16) | lo);
                    s = s.wrapping_add(src_modifier.wrapping_mul(2));
                    d = d.wrapping_add(dst_modifier);
                }
            }
            2 => {
                // 8/48 packing
                for _ in 0..(src_count / 6) {
                    let mut v = 0u64;
                    for k in 0..6u32 {
                        let b = self.dm_read32(bus, s.wrapping_add(k)) & 0xff;
                        v |= (b as u64) << (k * 8);
                    }
                    self.pm_write48(bus, d, v);
                    s = s.wrapping_add(src_modifier.wrapping_mul(6));
                    d = d.wrapping_add(dst_modifier);
                }
            }
            _ => {}
        }

        // Completion: latch in IRPTL, and request the interrupt if unmasked.
        let bit = 1u32 << (channel + 10);
        self.irptl |= bit;
        if self.imask & bit != 0 {
            self.irq_pending |= bit;
        }
    }

    /// The i960's window onto the SHARC's I/O processor registers (Model 2B
    /// maps it at 0x008c0000). With SYSCON's host-packing bit set the host bus
    /// is 16 bits wide, so two writes assemble one 32-bit value. Writing 0x1c
    /// loads DMA channel 6's control register; anything else lands in data
    /// memory.
    pub fn external_iop_write<B: SharcBus>(&mut self, bus: &mut B, address: u32, data: u32) {
        if self.syscon & 0x10 != 0 && address != 0x04 {
            let first = self.iop_write_num & 1 == 0;
            self.iop_write_num = self.iop_write_num.wrapping_add(1);
            if first {
                self.iop_data = data & 0xffff;
                return;
            }
            self.iop_data |= (data & 0xffff) << 16;
        } else {
            self.iop_data = data;
        }

        if address == 0x1c {
            self.dma[6].control = self.iop_data;
        } else {
            let v = self.iop_data;
            self.dm_write32(bus, address, v);
        }
    }

    /// The external DMA the i960 uses to upload the microcode (Model 2's
    /// `copro_fifo_w` in program-upload mode calls this per 16-bit word).
    /// DMA channel 6 in host boot mode uses 16/48 packing: three consecutive
    /// 16-bit words assemble one 48-bit program word, then the index advances.
    pub fn external_dma_write<B: SharcBus>(&mut self, bus: &mut B, address: u32, data: u32) {
        let index = (self.dma[6].int_index & 0x1ffff) | 0x20000;
        let mswf = (self.dma[6].control >> 8) & 1;
        let pmode = (self.dma[6].control >> 6) & 3;
        let dtype = (self.dma[6].control >> 5) & 1;
        match pmode {
            0 => {
                // no packing
                if dtype != 0 {
                    self.pm_write32(bus, index, data);
                } else {
                    self.dm_write32(bus, index, data);
                }
                self.dma[6].int_index =
                    self.dma[6].int_index.wrapping_add(self.dma[6].int_modifier);
            }
            2 => {
                // 16/48 packing
                let word = address % 3;
                let shift = if mswf != 0 {
                    (2 - word) * 16
                } else {
                    word * 16
                };
                let mut r = self.pm_read48(bus, index);
                r &= !(0xffffu64 << shift);
                r |= ((data & 0xffff) as u64) << shift;
                self.pm_write48(bus, index, r);
                if word == 2 {
                    self.dma[6].int_index =
                        self.dma[6].int_index.wrapping_add(self.dma[6].int_modifier);
                }
            }
            _ => {}
        }
    }
}
