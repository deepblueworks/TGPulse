//! Sega Model 1 motherboard: NEC V60 memory map and board-level devices.
//!
//! This first milestone models the complete host-visible map, GLUE interrupts,
//! timers, dual-port I/O RAM, sound UART and V60/TGP host communication.

use std::collections::VecDeque;

use mb86233::{Mb86233, Mb86233Bus};
use v60::{Bus, V60};

use crate::config::{Config, Inputs};
use crate::loader::Model1Roms;
use crate::sound::SoundSystem;

pub const CPU_HZ: u32 = 16_000_000;
pub const CYCLES_PER_FRAME: i32 = 656 * 424;

/// V60 clock, which the other boards' rates are expressed against.
pub const V60_HZ: u32 = 16_000_000;

/// Depth of the V60<->TGP hand-off FIFOs (`copro_fifo_in`/`_out`), which the reference
/// configures at 16 words each. The producer is paused once its FIFO is full so
/// the queues stay bounded instead of the V60 flooding the TGP.
pub const COPRO_FIFO_DEPTH: usize = 16;

const IO_STATUS: usize = 0x21;

pub struct Model1System {
    pub main_cpu: V60,
    pub tgp_cpu: Mb86233,

    pub maincpu_rom: Vec<u8>,
    pub nvram: Vec<u8>,
    pub work_ram: Vec<u8>,
    pub display_list: [Vec<u8>; 2],
    pub(crate) video: crate::model1_video::Model1VideoState,
    pub tile_ram: Vec<u8>,
    pub char_ram: Vec<u8>,
    pub palette_ram: Vec<u8>,
    pub colorxlat_ram: Vec<u8>,
    /// The I/O board: a Z80 with its own firmware, which owns the dual-port
    /// RAM it shares with the V60 and the 93C45 the operator settings live in.
    pub ioboard: crate::model1io::IoBoard,

    pub sound: SoundSystem,
    pub inputs: Inputs,
    pub drive_cmd: u8,
    pub config: Config,

    pub tgp_program: Vec<u32>,
    pub tgp_data: Vec<u32>,
    pub copro_ram: Vec<u32>,
    pub copro_fifo_in: VecDeque<u32>,
    pub copro_fifo_out: VecDeque<u32>,

    /// CPU-board math ROM the TGP's geometry accelerators index into.
    pub copro_tables: Vec<u32>,
    /// Geometry-board polygon/model ROM used by the Model 1 renderer.
    pub polygons: Vec<u32>,
    /// TGP external data ROM selected by `copro_data_base`.
    pub copro_data: Vec<u32>,
    /// The four TGP-side coprocessor-RAM address registers (I/O 0x00/08/10/18),
    /// each auto-incrementing on data access. Distinct from the V60-side
    /// `copro_ram_addr`, but they walk the same `copro_ram`.
    copro_io_ram_adr: [u32; 4],
    /// Latched arguments for the I/O-mapped function units.
    copro_sincos_base: u32,
    copro_inv_base: u32,
    copro_isqrt_base: u32,
    copro_atan_base: [u32; 4],
    copro_data_base: u32,

    pub listctl: [u16; 2],
    pub bank_reg: u16,
    pub bank_base: u32,

    pub irq_status: u8,
    pub irq_mask: u8,
    pub last_irq: u8,

    pub timer_mode: u16,
    pub timer_period: [u16; 2],
    pub timer_remaining: [u32; 2],

    pub frame_num: u64,

    pub copro_ram_addr: u16,
    copro_ram_latch: [u16; 2],
    copro_fifo_read_latch: u32,
    copro_fifo_write_latch: u32,
    copro_stall: bool,
    /// Ring of the last copro FIFO transfers (direction tag, value, in/out
    /// depths after the transfer). Always recording (it is only 128 slots) so
    /// a protocol stall can be autopsied after the fact.
    pub fifo_events: VecDeque<(char, u32, usize, usize)>,
}

impl Model1System {
    /// Where the I/O board's 93C45 mirror lives inside the dual-port RAM: one
    /// config byte per word of the 64-word EEPROM, starting with the "SEGA"
    /// magic. The game reads its operator settings (region included) from this
    /// copy, so it is the half of the machine's persistent state a settings
    /// change has to reach.
    const DPRAM_CONFIG: std::ops::Range<usize> = 0x100..0x140;

    /// Battery-backed SRAM at 0x400000, plus the I/O board's 93C45 image as
    /// the second block. The Model 1 boards have no EEPROM of their own, but
    /// the I/O board does, and the game keeps its operator settings there.
    pub fn nvram_blocks(&self) -> (Vec<u8>, Vec<u8>) {
        let eeprom = self
            .ioboard
            .eeprom()
            .data
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        (self.nvram.clone(), eeprom)
    }

    pub fn set_nvram_blocks(&mut self, backup: &[u8], eeprom: &[u8]) {
        let n = self.nvram.len().min(backup.len());
        self.nvram[..n].copy_from_slice(&backup[..n]);
        let chip = self.ioboard.eeprom_mut();
        for (word, chunk) in eeprom.chunks_exact(2).enumerate() {
            if word < chip.data.len() {
                chip.data[word] = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }
    }

    pub fn nvram_sizes(&self) -> (usize, usize) {
        (self.nvram.len(), Self::DPRAM_CONFIG.len())
    }

    pub fn new(roms: &Model1Roms) -> Self {
        Self::with_config(roms, Config::default())
    }

    pub fn with_config(roms: &Model1Roms, config: Config) -> Self {
        let mut video = crate::model1_video::Model1VideoState::new();
        video.smooth_shadows = config.smooth_shadows;
        // The 93C45 on the I/O board holds the operator settings. The romset
        // carries a dump of a configured one; a blank chip would send the game
        // into its setup menu on a first boot. The dump stores each 16-bit
        // word little-endian, which is also how the saved image keeps it.
        let mut eeprom = crate::eeprom93c46::Eeprom93c46::new();
        for (word, chunk) in roms.ioboard_config.chunks_exact(2).enumerate() {
            if word < eeprom.data.len() {
                eeprom.data[word] = u16::from_le_bytes([chunk[0], chunk[1]]);
            }
        }
        let mut ioboard = crate::model1io::IoBoard::new(&roms.iocpu, eeprom);
        ioboard.dpram_mut()[IO_STATUS] = 0x40;

        // Battery-backed work RAM (RAMA). The reference maps this as a plain zero-filled
        // RAM share with no shipped default, so the games initialise every byte
        // they rely on during boot. We must match that power-on state: a stray
        // 0xff fill leaves flags like vf's 0x40bf00 (a "skip the vblank game
        // logic" gate, read at FE3F21) set, so vf's ISR never builds a display
        // list and the screen stays blank. The couple of bytes we used to seed
        // for vr are unnecessary once the region starts at zero.
        let nvram = vec![0x00; 0x10000];

        Self {
            main_cpu: V60::new(),
            tgp_cpu: Mb86233::new(),

            maincpu_rom: roms.maincpu.clone(),
            nvram,
            work_ram: vec![0; 0x40000],
            display_list: [vec![0; 0x10000], vec![0; 0x10000]],
            video,
            tile_ram: vec![0; 0x10000],
            char_ram: vec![0; 0x80000],
            palette_ram: vec![0; 0x4000],
            colorxlat_ram: vec![0; 0xc000],
            ioboard,

            sound: SoundSystem::new(roms.sndcpu.clone(), roms.mpcm1.clone(), roms.mpcm2.clone()),
            inputs: Inputs::default(),
            drive_cmd: 0,
            config,

            tgp_program: roms
                .tgp
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
            tgp_data: vec![0; 0x400],
            copro_ram: vec![0; 0x2000],
            copro_fifo_in: VecDeque::new(),
            copro_fifo_out: VecDeque::new(),

            copro_tables: roms.copro_tables.clone(),
            polygons: roms.polygons.clone(),
            copro_data: roms.copro_data.clone(),
            copro_io_ram_adr: [0; 4],
            copro_sincos_base: 0,
            copro_inv_base: 0,
            copro_isqrt_base: 0,
            copro_atan_base: [0; 4],
            copro_data_base: 0,

            listctl: [0; 2],
            bank_reg: 0x0001,
            bank_base: 0x0100_0000,

            irq_status: 0,
            irq_mask: 0xff,
            last_irq: 0,

            timer_mode: 0,
            timer_period: [0; 2],
            timer_remaining: [0; 2],

            frame_num: 0,

            copro_ram_addr: 0,
            copro_ram_latch: [0; 2],
            copro_fifo_read_latch: 0,
            copro_fifo_write_latch: 0,
            copro_stall: false,
            fifo_events: VecDeque::new(),
        }
    }

    pub fn run_slice(&mut self, cycles: i32) {
        const QUANTUM: i32 = 64;

        let mut remaining = cycles;
        while remaining > 0 {
            let step = remaining.min(QUANTUM);

            // Pause the V60 while its outbound FIFO is full, so it cannot outrun
            // the TGP and the queue stays near the hardware's 16-word depth.
            if self.copro_fifo_in.len() <= COPRO_FIFO_DEPTH {
                self.sync_irq();
                let mut cpu = std::mem::replace(&mut self.main_cpu, V60::new());
                cpu.run(self, step);
                self.main_cpu = cpu;
                self.sync_irq();
            }

            // The I/O board runs alongside. Its Z80 is clocked at 4 MHz against
            // the V60's 16 MHz, so it advances a quarter as far.
            self.ioboard.set_inputs(self.inputs);
            self.ioboard
                .run(step as i64 * crate::model1io::Z80_HZ as i64 / V60_HZ as i64);
            self.drive_cmd = self.ioboard.drive_cmd();

            // Step the TGP in the same fine lockstep so the FIFO handshakes
            // resolve instead of deadlocking. The MB86233 runs at 40 MHz against
            // the V60's 16 MHz, so it advances ~2.5x as far per slice; a FIFO
            // read with no data rewinds and retries (take_stall), which is how
            // it waits for the V60 without an event scheduler. It halts itself
            // (halt_requested) once its own output FIFO backs up.
            if self.copro_fifo_out.len() <= COPRO_FIFO_DEPTH {
                let tgp_step = (step * 5) / 2;
                let mut tgp = std::mem::replace(&mut self.tgp_cpu, Mb86233::new());
                tgp.execute(self, tgp_step);
                self.tgp_cpu = tgp;
            }

            self.advance_timers(step as u32);
            self.sound.run(step, CPU_HZ);
            // sound_ready_w: the M1 audio UART's ready lines
            // raise IRQ level 3 while unmasked -- vf's sound-queue pump lives
            // in that handler and the game hangs in its boot without it.
            if (self.sound.board.uart_tx_ready() || self.sound.board.uart_rx_ready())
                && self.irq_mask & (1 << 3) == 0
            {
                self.raise_irq(3);
            }

            remaining -= step;
        }
    }

    pub fn run_frame(&mut self) {
        self.run_slice(CYCLES_PER_FRAME);
        self.trigger_vblank();
    }

    pub fn trigger_vblank(&mut self) {
        // set_current_render_list: when bit 2 of listctl[0]
        // is clear, the display-buffer select (bit 6) is latched from bit 3 at
        // render time. The V60 reads bit 6 back to learn which buffer to write
        // next (get_list_number), so the latch must be visible to the CPU, not
        // just to the renderer. end_frame: in bit-2 mode the hardware toggles
        // the select itself every other frame.
        if self.listctl[0] & 4 == 0 {
            self.listctl[0] =
                (self.listctl[0] & !0x40) | if self.listctl[0] & 8 != 0 { 0x40 } else { 0 };
        } else if self.frame_num & 1 != 0 {
            self.listctl[0] ^= 0x40;
        }
        // The reference scans renderer uploads on the rising vblank edge even when the
        // display list was not rasterized during that frame.
        crate::model1_video::scan_uploads(self);
        self.frame_num = self.frame_num.wrapping_add(1);
        if self.irq_mask & (1 << 1) == 0 {
            self.raise_irq(1);
        }
    }

    pub fn raise_irq(&mut self, level: u8) {
        self.irq_status |= 1 << level;
    }

    fn sync_irq(&mut self) {
        if let Some(level) = (0..8).find(|level| self.irq_status & (1 << level) != 0) {
            self.last_irq = level;
            self.main_cpu.assert_irq(level);
        } else {
            self.main_cpu.clear_irq();
        }
    }

    fn irq_control_w(&mut self, value: u8) {
        match value {
            0x10 => self.irq_status = 0,
            0x20 => self.irq_status &= !(1 << self.last_irq),
            _ => {}
        }
    }

    fn bank_w(&mut self, value: u16) {
        self.bank_reg = value;
        if value & 0x0f == 1 {
            self.bank_base = 0x0100_0000 + 0x0010_0000 * ((value as u32 >> 4) & 7);
        }
    }

    fn set_timer_period(&mut self, index: usize, value: u16) {
        self.timer_period[index] = value;
        self.timer_remaining[index] = u32::from(value) * 0x800;
    }

    fn advance_timers(&mut self, cycles: u32) {
        for index in 0..2 {
            let remaining = self.timer_remaining[index];
            if remaining == 0 {
                continue;
            }

            if cycles < remaining {
                self.timer_remaining[index] -= cycles;
                continue;
            }

            if self.irq_mask & 1 == 0 {
                self.raise_irq(0);
            }
            self.timer_remaining[index] = u32::from(self.timer_period[index]) * 0x800;
        }
    }

    fn timer_r(&self, index: usize) -> u16 {
        (self.timer_remaining[index] / 0x800) as u16
    }

    fn dpram_write(&mut self, index: usize, value: u8) {
        if let Some(dst) = self.ioboard.dpram_mut().get_mut(index) {
            *dst = value;
        }
    }

    fn copro_ram_read(&mut self, high: bool) -> u16 {
        let value = self.copro_ram[(self.copro_ram_addr & 0x1fff) as usize];
        let result = if high {
            (value >> 16) as u16
        } else {
            value as u16
        };

        if high && self.copro_ram_addr & 0x8000 != 0 {
            self.copro_ram_addr = self.copro_ram_addr.wrapping_add(1);
        }
        result
    }

    fn copro_ram_write(&mut self, high: bool, value: u16) {
        self.copro_ram_latch[usize::from(high)] = value;
        if high {
            let word =
                u32::from(self.copro_ram_latch[0]) | (u32::from(self.copro_ram_latch[1]) << 16);
            let index = (self.copro_ram_addr & 0x1fff) as usize;
            self.copro_ram[index] = word;
            if self.copro_ram_addr & 0x8000 != 0 {
                self.copro_ram_addr = self.copro_ram_addr.wrapping_add(1);
            }
        }
    }

    /// Records one copro FIFO transfer in the autopsy ring (128 slots).
    /// Empty-pop stall retries are not transfers and are not recorded.
    fn fifo_note(&mut self, dir: char, value: u32) {
        if self.fifo_events.len() >= 128 {
            self.fifo_events.pop_front();
        }
        let depths = (self.copro_fifo_in.len(), self.copro_fifo_out.len());
        self.fifo_events.push_back((dir, value, depths.0, depths.1));
    }

    /// The reference stalls a V60 read of an empty output FIFO until the TGP delivers
    /// a word. Our
    /// scheduler runs the V60 ahead of the TGP within a quantum, so the read
    /// would otherwise consume a stale or bogus word. Pump the TGP until it
    /// produces the result instead -- the same effect as the hardware stall.
    fn copro_fifo_pump(&mut self) {
        for _ in 0..2000 {
            if !self.copro_fifo_out.is_empty() {
                break;
            }
            let retries_before = self.tgp_cpu.cov.stall_retries;
            let mut tgp = std::mem::replace(&mut self.tgp_cpu, Mb86233::new());
            tgp.execute(self, 64);
            self.tgp_cpu = tgp;
            if self.copro_fifo_out.is_empty()
                && self.copro_fifo_in.is_empty()
                && self.tgp_cpu.cov.stall_retries != retries_before
            {
                // The TGP is waiting for host input; it cannot produce a word
                // no matter how long we pump. Give up and return 0.
                break;
            }
        }
    }

    fn copro_fifo_read(&mut self, high: bool) -> u16 {
        if !high {
            if self.copro_fifo_out.is_empty() {
                self.copro_fifo_pump();
            }
            self.copro_fifo_read_latch = self.copro_fifo_out.pop_front().unwrap_or(0);
            self.fifo_note('R', self.copro_fifo_read_latch);
            log::trace!(target: "fifo",
                "[fifo] V60 {:06X} pop  out={:08X} (out left {})",
                self.main_cpu.pc() & 0x00ff_ffff,
                self.copro_fifo_read_latch,
                self.copro_fifo_out.len()
            );
            self.copro_fifo_read_latch as u16
        } else {
            (self.copro_fifo_read_latch >> 16) as u16
        }
    }

    fn copro_fifo_write(&mut self, high: bool, value: u16) {
        if high {
            self.copro_fifo_write_latch =
                (self.copro_fifo_write_latch & 0x0000_ffff) | (u32::from(value) << 16);
            self.copro_fifo_in.push_back(self.copro_fifo_write_latch);
            self.fifo_note('W', self.copro_fifo_write_latch);
            log::trace!(target: "fifo",
                "[fifo] V60 {:06X} push in={:08X} (in {})",
                self.main_cpu.pc() & 0x00ff_ffff,
                self.copro_fifo_write_latch,
                self.copro_fifo_in.len()
            );
        } else {
            self.copro_fifo_write_latch =
                (self.copro_fifo_write_latch & 0xffff_0000) | u32::from(value);
        }
    }

    // --- TGP coprocessor I/O map
    // model1_m.cpp. The address registers, the RAM data window and the geometry
    // function units (sincos/atan/inv/isqrt) all read `copro_tables`/`copro_ram`.

    /// Advance one of the four RAM address registers after a data access: page 4
    /// (bit 0x40000) strides by 4, everything else by 1.
    fn copro_ramadr_step(&mut self, reg: usize) {
        let adr = self.copro_io_ram_adr[reg];
        self.copro_io_ram_adr[reg] = adr.wrapping_add(if adr & 0x40000 != 0 { 4 } else { 1 });
    }

    fn copro_ramdata_r(&mut self, reg: usize) -> u32 {
        let val = self.copro_ram[(self.copro_io_ram_adr[reg] & 0x1fff) as usize];
        self.copro_ramadr_step(reg);
        val
    }

    fn copro_ramdata_w(&mut self, reg: usize, data: u32) {
        let index = (self.copro_io_ram_adr[reg] & 0x1fff) as usize;
        self.copro_ram[index] = data;
        self.copro_ramadr_step(reg);
    }

    fn copro_sincos_r(&self, offset: u32) -> u32 {
        let ang = self.copro_sincos_base.wrapping_add(offset * 0x4000);
        let mut index = (ang & 0x3fff) as i32;
        if ang & 0x4000 != 0 {
            index = (0x4000 - index).min(0x3fff);
        }
        let mut result = self.copro_tables[index as usize];
        if ang & 0x8000 != 0 {
            result ^= 0x8000_0000;
        }
        result
    }

    fn copro_inv_r(&self, offset: u32) -> u32 {
        let index = ((self.copro_inv_base >> 9) & 0x3ffe) | (offset & 1);
        let result = self.copro_tables[(index | 0x8000) as usize];
        let bexp = (self.copro_inv_base >> 23) & 0xff;
        let exp = (result >> 23).wrapping_add(0x7f).wrapping_sub(bexp) & 0xff;
        let mut result = (result & 0x807f_ffff) | (exp << 23);
        if self.copro_inv_base & 0x8000_0000 != 0 {
            result ^= 0x8000_0000;
        }
        result
    }

    fn copro_isqrt_r(&self, offset: u32) -> u32 {
        let index = 0x2000 ^ (((self.copro_isqrt_base >> 10) & 0x3ffe) | (offset & 1));
        let result = self.copro_tables[(index | 0xc000) as usize];
        let bexp = (self.copro_isqrt_base >> 24) & 0x7f;
        let exp = (result >> 23).wrapping_add(0x3f).wrapping_sub(bexp) & 0xff;
        let mut result = (result & 0x807f_ffff) | (exp << 23);
        if offset & 1 == 0 {
            result &= 0x7fff_ffff;
        }
        result
    }

    fn copro_atan_r(&self) -> u32 {
        let mut idx = self.copro_atan_base[3] & 0xffff;
        if idx & 0xc000 != 0 {
            idx = 0x3fff;
        }
        let mut result = self.copro_tables[(idx | 0x4000) as usize];

        // Correct a known table bug the way the hardware effectively does.
        let dt = (result >> 16).wrapping_add(result) as u16;
        if dt & 0x001 != 0 {
            result = if result & 0x00f == 0x00e {
                result.wrapping_sub(0x0000_0001)
            } else {
                result.wrapping_sub(0x0001_0000)
            };
        }
        if dt & 0x010 != 0 {
            result = if result & 0x0f0 == 0x0e0 {
                result.wrapping_sub(0x0000_0010)
            } else {
                result.wrapping_sub(0x0010_0000)
            };
        }
        if dt & 0x100 != 0 {
            result = if result & 0xf00 == 0xe00 {
                result.wrapping_sub(0x0000_0100)
            } else {
                result.wrapping_sub(0x0100_0000)
            };
        }

        let s0 = self.copro_atan_base[0] & 0x8000_0000 != 0;
        let s1 = self.copro_atan_base[1] & 0x8000_0000 != 0;
        let s2 = self.copro_atan_base[2] & 0x8000_0000 != 0;
        if s0 ^ s1 ^ s2 {
            result >>= 16;
        }
        if s2 {
            result = result.wrapping_add(0x4000);
        }
        if (s0 && !s2) || (s1 && s2) {
            result = result.wrapping_add(0x8000);
        }
        result & 0xffff
    }

    fn copro_io_read(&mut self, address: u32) -> u32 {
        self.copro_io_read_inner(address)
    }

    fn copro_io_read_inner(&mut self, address: u32) -> u32 {
        match address {
            // RAM address / data registers, four banks selected by bits 3-4.
            a if a < 0x20 && a & 1 == 0 => {
                let v = self.copro_io_ram_adr[(a >> 3) as usize & 3];
                // The TGP writes 0xFFFFFFFF here as an "end of results" sentinel
                // (its auto-incrementing RAM pointer then wraps to slot 0). When the
                // microcode's polygon-count read lands on this register it would
                // otherwise take that sentinel as a count of ~4 billion and hang.
                // A sentinel is never a real count, so report it as empty (0), which
                // is what every count-0 object already does via unmapped addresses.
                if v == 0xFFFF_FFFF {
                    0
                } else {
                    v
                }
            }
            a if a < 0x20 => self.copro_ramdata_r((a >> 3) as usize & 3),
            0x20..=0x23 => self.copro_sincos_r(address - 0x20),
            0x24..=0x27 => self.copro_atan_r(),
            0x28..=0x29 => self.copro_inv_r(address - 0x28),
            0x2a..=0x2b => self.copro_isqrt_r(address & 1),
            0x8000..=0xffff => {
                // The reference copro_data_r receives a window-relative offset from
                // 0x0000 through 0x7fff, not the absolute I/O address.
                if self.copro_data.is_empty() {
                    0
                } else {
                    let offset = address & 0x7fff;
                    let index = (self.copro_data_base & !0x7fff) | offset;
                    self.copro_data[index as usize & (self.copro_data.len() - 1)]
                }
            }
            _ => 0,
        }
    }

    fn copro_io_write(&mut self, address: u32, data: u32) {
        match address {
            a if a < 0x20 && a & 1 == 0 => self.copro_io_ram_adr[(a >> 3) as usize & 3] = data,
            a if a < 0x20 => self.copro_ramdata_w((a >> 3) as usize & 3, data),
            0x20..=0x23 => self.copro_sincos_base = data,
            0x24..=0x27 => self.copro_atan_base[(address - 0x24) as usize] = data,
            0x28..=0x29 => self.copro_inv_base = data,
            0x2a..=0x2b => self.copro_isqrt_base = data,
            0x2e => self.copro_data_base = data,
            _ => {}
        }
    }

    fn normal_read(&mut self, address: u32) -> u8 {
        let address = address & 0x00ff_ffff;
        match address {
            0x000000..=0x0fffff | 0x200000..=0x2fffff | 0xf80000..=0xffffff => self
                .maincpu_rom
                .get(address as usize)
                .copied()
                .unwrap_or(0xff),
            0x100000..=0x1fffff => {
                let offset = self.bank_base + address - 0x100000;
                self.maincpu_rom
                    .get(offset as usize)
                    .copied()
                    .unwrap_or(0xff)
            }
            0x400000..=0x40ffff => self.nvram[(address - 0x400000) as usize],
            0x500000..=0x53ffff => self.work_ram[(address - 0x500000) as usize],
            0x600000..=0x60ffff => self.display_list[0][(address - 0x600000) as usize],
            0x610000..=0x61ffff => self.display_list[1][(address - 0x610000) as usize],
            0x680000..=0x680003 => {
                let index = ((address - 0x680000) >> 1) as usize;
                let mut value = self.listctl[index];
                if index == 0 {
                    value |= 0x30;
                }
                (value >> ((address & 1) * 8)) as u8
            }
            0x700000..=0x70ffff => self.tile_ram[(address - 0x700000) as usize],
            0x780000..=0x7fffff => self.char_ram[(address - 0x780000) as usize],
            0x900000..=0x903fff => self.palette_ram[(address - 0x900000) as usize],
            0x910000..=0x91bfff => self.colorxlat_ram[(address - 0x910000) as usize],
            0xc00000..=0xc00fff if address & 1 == 0 => {
                let idx = ((address - 0xc00000) >> 1) as usize & 0x7ff;

                self.ioboard.dpram()[idx]
            }
            0xc40000 => self.sound.board.tx.take().unwrap_or(0),
            0xc40002 => {
                let mut status = 0x05;
                if self.sound.board.uart_rx_ready() {
                    status |= 0x02;
                }
                status
            }
            0xe00002 => self.irq_mask,
            0xe0000c..=0xe0000f => {
                let index = ((address - 0xe0000c) >> 1) as usize;
                (self.timer_r(index) >> ((address & 1) * 8)) as u8
            }
            _ => 0xff,
        }
    }

    fn normal_write(&mut self, address: u32, value: u8) {
        let address = address & 0x00ff_ffff;
        match address {
            0x400000..=0x40ffff => self.nvram[(address - 0x400000) as usize] = value,
            0x500000..=0x53ffff => self.work_ram[(address - 0x500000) as usize] = value,
            0x600000..=0x60ffff => self.display_list[0][(address - 0x600000) as usize] = value,
            0x610000..=0x61ffff => self.display_list[1][(address - 0x610000) as usize] = value,
            0x680000..=0x680003 => {
                let index = ((address - 0x680000) >> 1) as usize;
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                self.listctl[index] = (self.listctl[index] & !mask) | (u16::from(value) << shift);
            }
            0x700000..=0x70ffff => self.tile_ram[(address - 0x700000) as usize] = value,
            0x780000..=0x7fffff => self.char_ram[(address - 0x780000) as usize] = value,
            0x900000..=0x903fff => self.palette_ram[(address - 0x900000) as usize] = value,
            0x910000..=0x91bfff => self.colorxlat_ram[(address - 0x910000) as usize] = value,
            0xc00000..=0xc00fff if address & 1 == 0 => {
                let index = ((address - 0xc00000) >> 1) as usize & 0x7ff;
                self.dpram_write(index, value);
            }
            0xc40000 => self.sound.send(value),
            0xc40002 => self.sound.board.uart_control(value),
            0xe00000 => self.irq_control_w(value),
            0xe00002 => self.irq_mask = value,
            0xe00004..=0xe00005 => {
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                let combined = (self.bank_reg & !mask) | (u16::from(value) << shift);
                self.bank_w(combined);
            }
            0xe00006..=0xe00007 => {
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                self.timer_mode = (self.timer_mode & !mask) | (u16::from(value) << shift);
            }
            0xe00008..=0xe0000b => {
                let index = ((address - 0xe00008) >> 1) as usize;
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                let combined = (self.timer_period[index] & !mask) | (u16::from(value) << shift);
                self.set_timer_period(index, combined);
            }
            _ => {}
        }
    }
}

impl Bus for Model1System {
    /// Live GLUE interrupt line: asserted while any raised level is still
    /// pending. The ISR's `E00000` acknowledge clears `irq_status` mid-run, so
    /// the CPU consults this instead of re-taking its latched line after `reti`.
    fn irq_active(&self) -> Option<bool> {
        Some(self.irq_status != 0)
    }

    fn read_u8(&mut self, address: u32) -> u8 {
        let address = address & 0x00ff_ffff;
        let word = match address {
            0xd00000..=0xd1ffff => self.copro_ram_addr,
            0xd20000..=0xd3ffff => self.copro_ram_read(address & 2 != 0),
            0xd80000..=0xd9ffff => self.copro_fifo_read(address & 2 != 0),
            0xdc0000..=0xddffff => 0xffff,
            _ => return self.normal_read(address),
        };
        (word >> ((address & 1) * 8)) as u8
    }

    fn write_u8(&mut self, address: u32, value: u8) {
        let address = address & 0x00ff_ffff;
        match address {
            0xd00000..=0xd1ffff => {
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                self.copro_ram_addr = (self.copro_ram_addr & !mask) | (u16::from(value) << shift);
            }
            0xd20000..=0xd3ffff => {
                let high = address & 2 != 0;
                let index = usize::from(high);
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                let combined = (self.copro_ram_latch[index] & !mask) | (u16::from(value) << shift);
                self.copro_ram_write(high, combined);
            }
            0xd80000..=0xd9ffff => {
                let high = address & 2 != 0;
                let current = if high {
                    (self.copro_fifo_write_latch >> 16) as u16
                } else {
                    self.copro_fifo_write_latch as u16
                };
                let shift = (address & 1) * 8;
                let mask = 0xffu16 << shift;
                self.copro_fifo_write(high, (current & !mask) | (u16::from(value) << shift));
            }
            _ => self.normal_write(address, value),
        }
    }

    fn read_u16(&mut self, address: u32) -> u16 {
        let address = address & 0x00ff_ffff;
        match address {
            0xd00000..=0xd1ffff => self.copro_ram_addr,
            0xd20000..=0xd3ffff => self.copro_ram_read(address & 2 != 0),
            0xd80000..=0xd9ffff => self.copro_fifo_read(address & 2 != 0),
            0xdc0000..=0xddffff => 0xffff,
            _ => u16::from_le_bytes([
                self.normal_read(address),
                self.normal_read(address.wrapping_add(1)),
            ]),
        }
    }

    fn write_u16(&mut self, address: u32, value: u16) {
        let address = address & 0x00ff_ffff;
        match address {
            0xd00000..=0xd1ffff => self.copro_ram_addr = value,
            0xd20000..=0xd3ffff => self.copro_ram_write(address & 2 != 0, value),
            0xd80000..=0xd9ffff => self.copro_fifo_write(address & 2 != 0, value),
            0xe00004..=0xe00005 => self.bank_w(value),
            0xe00006..=0xe00007 => self.timer_mode = value,
            0xe00008..=0xe00009 => self.set_timer_period(0, value),
            0xe0000a..=0xe0000b => self.set_timer_period(1, value),
            _ => {
                let bytes = value.to_le_bytes();
                self.normal_write(address, bytes[0]);
                self.normal_write(address.wrapping_add(1), bytes[1]);
            }
        }
    }

    fn read_u32(&mut self, address: u32) -> u32 {
        let lo = u32::from(self.read_u16(address));
        let hi = u32::from(self.read_u16(address.wrapping_add(2)));
        lo | (hi << 16)
    }

    fn write_u32(&mut self, address: u32, value: u32) {
        self.write_u16(address, value as u16);
        self.write_u16(address.wrapping_add(2), (value >> 16) as u16);
    }

    fn halt_requested(&self) -> bool {
        // The full-post-sync callback
        // asserts INPUT_LINE_HALT on the V60 after accepting the overflow word.
        self.copro_fifo_in.len() > COPRO_FIFO_DEPTH
    }
}

impl crate::tilemap::TileSource for Model1System {
    fn tile_u16(&self, idx: usize) -> u16 {
        le16(&self.tile_ram, idx)
    }
    fn char_word(&self, idx: usize) -> u32 {
        le32(&self.char_ram, idx)
    }
    fn palette_u16(&self, idx: usize) -> u16 {
        le16(&self.palette_ram, idx)
    }
    fn colorxlat_u16(&self, idx: usize) -> u16 {
        le16(&self.colorxlat_ram, idx)
    }
    fn colorxlat_written(&self) -> bool {
        // VR programs the translation RAM, but until that path is verified the
        // 5->8-bit expansion fallback keeps the picture legible.
        false
    }
    fn monitor_gamma(&self, v: u32) -> u32 {
        v & 0xff
    }
}

/// Little-endian u16 at u16-index `idx` in a byte-addressed RAM.
fn le16(mem: &[u8], idx: usize) -> u16 {
    let b = idx * 2;
    u16::from_le_bytes([
        mem.get(b).copied().unwrap_or(0),
        mem.get(b + 1).copied().unwrap_or(0),
    ])
}

/// Little-endian u32 at u32-index `idx` in a byte-addressed RAM.
fn le32(mem: &[u8], idx: usize) -> u32 {
    let b = idx * 4;
    u32::from_le_bytes([
        mem.get(b).copied().unwrap_or(0),
        mem.get(b + 1).copied().unwrap_or(0),
        mem.get(b + 2).copied().unwrap_or(0),
        mem.get(b + 3).copied().unwrap_or(0),
    ])
}

impl Mb86233Bus for Model1System {
    fn read_program(&mut self, address: u32) -> u32 {
        self.tgp_program.get(address as usize).copied().unwrap_or(0)
    }

    fn read_data(&mut self, address: u32) -> u32 {
        if address == 0x100 {
            return match self.copro_fifo_in.pop_front() {
                Some(value) => {
                    self.fifo_note('r', value);
                    log::trace!(target: "fifo",
                        "[fifo] TGP {:04X} pop  in={:08X} (in left {})",
                        self.tgp_cpu.pc, value, self.copro_fifo_in.len()
                    );
                    value
                }
                None => {
                    self.copro_stall = true;
                    0
                }
            };
        }
        self.tgp_data.get(address as usize).copied().unwrap_or(0)
    }

    fn write_data(&mut self, address: u32, value: u32) {
        if address == 0x400 {
            self.fifo_note('w', value);
            log::trace!(target: "fifo",
                "[fifo] TGP {:04X} push out={:08X} (out {})",
                self.tgp_cpu.pc, value, self.copro_fifo_out.len()
            );
            self.copro_fifo_out.push_back(value);
        } else if let Some(dst) = self.tgp_data.get_mut(address as usize) {
            *dst = value;
        }
    }

    fn read_io(&mut self, address: u32) -> u32 {
        self.copro_io_read(address)
    }

    fn write_io(&mut self, address: u32, value: u32) {
        self.copro_io_write(address, value);
    }

    /// The TGP fetches host commands through register-file port 1, the same
    /// wiring as Model 2: an empty FIFO stalls the instruction so it retries
    /// until the V60 supplies a word, rather than running on with a bogus 0.
    fn read_rf(&mut self, address: u32) -> u32 {
        match address {
            1 => match self.copro_fifo_in.pop_front() {
                Some(v) => {
                    self.fifo_note('r', v);
                    log::trace!(target: "fifo",
                        "[fifo] TGP {:04X} pop  in={:08X} (in left {}) [rf1]",
                        self.tgp_cpu.pc, v, self.copro_fifo_in.len()
                    );
                    v
                }
                None => {
                    self.copro_stall = true;
                    0
                }
            },
            _ => 0,
        }
    }

    /// Port 0 is the LED/busy latch (ignored); port 2 pushes a result word back
    /// to the V60 through the output FIFO.
    fn write_rf(&mut self, address: u32, value: u32) {
        if address == 2 {
            self.fifo_note('w', value);
            log::trace!(target: "fifo",
                "[fifo] TGP {:04X} push out={:08X} (out {}) [rf2]",
                self.tgp_cpu.pc, value, self.copro_fifo_out.len()
            );
            self.copro_fifo_out.push_back(value);
        }
    }

    fn take_stall(&mut self) -> bool {
        std::mem::take(&mut self.copro_stall)
    }

    fn halt_requested(&self) -> bool {
        // Match the FIFO semantics: accept the overflow word, then halt
        // before the TGP fetches and executes another instruction.
        self.copro_fifo_out.len() > COPRO_FIFO_DEPTH
    }
}
