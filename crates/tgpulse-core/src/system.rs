use i960::cpu::I960Cpu;
use mb86233::Mb86233;

use crate::config::{Cabinet, Config, Inputs};
use crate::geometry::GeometryEngine;
use crate::loader::Roms;
use crate::sound::{Sound, SoundSystem};

// --- Model 1 I/O board dual-port RAM layout (device byte offsets) ---
/// Command register: the main CPU writes a command here and spins until the
/// board zeroes it.
pub const IO_CMD: usize = 0x20;
/// Status register. The boot ROM requires this to read 0x40 before it will
/// issue its first command.
pub const IO_STATUS: usize = 0x21;

// Dual-port RAM layout published by the I/O board's command 1, recovered by
// disassembling its Z80 ROM (epr-14869c, handler at 0x00b3). The board walks
// the 5338A's ports and drops each one at a fixed offset:
//
//   0x00-0x03 ADC channels 0-3, primary controls
//   0x04-0x07 ADC channels 0-3, secondary controls
//   0x08-0x0a 5338A ports B/C/D, primary   = IN0, IN1, IN2
//   0x0b-0x0d 5338A ports B/C/D, secondary = DSW1, DSW2, DSW3
//   0x0e 5338A port G = eeprom DO | board buttons
pub const IO_STEER: usize = 0x00;
pub const IO_ACCEL: usize = 0x01;
pub const IO_BRAKE: usize = 0x02;
pub const IO_ANALOG3: usize = 0x03;
pub const IO_ANALOG_SECONDARY: usize = 0x04;
pub const IO_IN0: usize = 0x08;
pub const IO_IN1: usize = 0x09;
pub const IO_IN2: usize = 0x0A;
pub const IO_DSW1: usize = 0x0B;
pub const IO_DSW2: usize = 0x0C;
pub const IO_DSW3: usize = 0x0D;
/// Port G: bit 7 eeprom DO, bits 4-6 pulled high, bits 0-3 the on-board
/// buttons (active low). The reference: `io_pg_r`.
pub const IO_PORTG: usize = 0x0E;

// The board's output block, recovered from the same Z80 ROM at 0x0170..0x01b9.
// That code copies dpram bytes out to the 5338A rather than in:
//
//   0x10       -> port A, high nibble (shifted left by 4)
//   0x11       <-> port E, the byte exchanged with the drive (force feedback)
//                 board on the other end of the cable
//   0x12 bit 0 -> direction: 1 makes port E an input and publishes what the
//                 drive board said into 0x11; 0 makes it an output and sends
//                 0x11 to the drive board.
/// Data byte exchanged with the drive board.
pub const IO_DRIVE_DATA: usize = 0x11;
/// Direction control for `IO_DRIVE_DATA`.
pub const IO_DRIVE_DIR: usize = 0x12;

// --- Battery-backed settings block, at backup SRAM offset 0 ---
//
// Layout recovered by disassembling the boot code: the validator at 0x00228810,
// the rebuild at 0x00228e00, and the checksum helper at 0x002292d8.
//
//   0x00-0x07 signature, checked against the game's own ROM defaults
//   0x08-0x09 CRC-16/CCITT of 0x0a-0x7f, little endian
//   0x0a-0x7f the settings
//   0x0b link role (see `Cabinet::link_role`)
//   0x80-0xff the operator's saved copy of all of the above. The boot code
//              validates this and copies it over 0x00-0x7f, which is why the
//              test menu writes here and not to the working block.
//
/// Offset of the block's checksum.
const NV_CRC: usize = 0x08;
/// First byte the checksum covers.
const NV_DATA: usize = 0x0a;
/// One past the last byte the checksum covers.
const NV_END: usize = 0x80;
/// Link role byte, read by the boot code at 0x1200 to decide whether to run the
/// network check at all.
const NV_ROLE: usize = 0x0b;
/// Base of the operator's saved copy.
const NV_SAVED: usize = 0x80;

/// Transfer exponent of the cabinet's picture tube. A CRT's light output
/// follows a power law in grid drive; 2.5 is the standard figure for one.
///
/// This is the only number in the colour path that is not recovered from the
/// game: the pedestal comes from the game's own curve, but the tube's exponent
/// is a property of the glass and Sega did not write it into the ROM. It is
/// left visible here rather than folded into a magic constant.
const CRT_GAMMA: f32 = 2.5;
/// Exponent a modern display decodes with. Whatever we hand it is raised to
/// this, so only the *ratio* of the two matters: emitting `s^(2.5/2.2)` makes
/// an sRGB panel produce the `s^2.5` the tube would have.
const SRGB_GAMMA: f32 = 2.2;

/// Standard CRC-16/CCITT table (poly 0x1021), built once at first use.
static CRC16_CCITT: std::sync::LazyLock<[u16; 256]> = std::sync::LazyLock::new(|| {
    let mut t = [0u16; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = (i as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x1021
            } else {
                c << 1
            };
        }
        *e = c;
    }
    t
});

/// Depth of both coprocessor FIFOs, eight words each. The depth is
/// load-bearing: the hardware
/// halts the TGP when the output FIFO fills, which is the only thing stopping
/// it from free-running.
pub const COPRO_FIFO_DEPTH: usize = 8;

/// M2COMM frame offset, 0x1c0 on all Model 2 games except Power Sled.
pub const COMM_FRAME_OFFSET: u16 = 0x01c0;

pub struct Model2System {
    // --- Devices ---
    // Boxed, and paired with a spare below: `execute_run` needs `&mut self`,
    // so the running core has to be moved out of the system for the duration
    // of its slice. Moving the structs themselves cost two memcpys of the
    // whole register file plus a fresh construction, every 64-cycle quantum --
    // which profiled at roughly three quarters of total run time. Boxing makes
    // each of those a pointer move.
    pub main_cpu: Box<I960Cpu>,
    pub tgp_cpu: Box<Mb86233>,
    /// The Model 2B geometry coprocessor (ADSP-21062 SHARC). Present only on
    /// 2B games; the original/2A boards use `tgp_cpu`. Boxed and parked the
    /// same way, since running it also needs `&mut self` as its bus.
    pub sharc: Box<sharc::Sharc>,
    /// The Model 2C geometry coprocessor (Fujitsu MB86235 "TGPx4").
    pub tgpx4: Box<mb86235::Mb86235>,
    /// The board's coprocessor kind, which selects `tgp_cpu` vs `sharc`.
    pub coprocessor: crate::roms_db::Board,
    /// SHARC external-bus access tallies, for 2B bring-up.
    pub sharc_reads: u64,
    pub sharc_writes: u64,
    pub sharc_read_addrs: [u64; 4],
    pub sharc_write_addrs: [u64; 4],
    pub sharc_write_samples: [u32; 8],
    /// Sega 315-5649 I/O chip state (2A/2B/2C boards): the output-port latches
    /// and the auto-incrementing analog channel selector.
    pub io5649_ports: [u8; 8],
    pub io5649_analog: u32,
    /// Mux index the gun interface board will answer with on serial ch2.
    pub io5649_gun_mux: u32,
    /// 93C46 hanging off the I/O chip's port A (2A/2B/2C).
    pub eeprom: crate::eeprom93c46::Eeprom93c46,
    /// Port A bit 0: when set, port B reports the EEPROM data line instead of
    /// the cabinet's IN0 switches.
    pub io5649_ctrlmode: bool,
    pub tgpx4_pops: u64,
    pub tgpx4_pushes: u64,
    pub tgpx4_ext_r: u64,
    pub tgpx4_ext_w: u64,
    pub tgpx4_rbucket: [u64; 3],
    pub tgpx4_rsample: [u32; 8],
    pub geo_pushes: u64,

    /// Placeholders left in `main_cpu`/`tgp_cpu` while the running core runs.
    /// Bus writes made during a slice land on these and are discarded; they
    /// are never promoted to the active core, so carrying them across quanta
    /// (rather than building a fresh one each time) changes nothing.
    parked_main: Option<Box<I960Cpu>>,
    pub(crate) parked_tgp: Option<Box<Mb86233>>,
    pub(crate) parked_sharc: Option<Box<sharc::Sharc>>,
    pub(crate) parked_tgpx4: Option<Box<mb86235::Mb86235>>,

    /// The coprocessor worker thread, when `Config::multithreaded` is set.
    /// Owns the board's DSP core and the authoritative FIFOs; the fields
    /// below keep their single-threaded meaning and are resynced from the
    /// worker for savestates and the debugger.
    pub copro_mt: Option<crate::copro::CoproWorker>,

    // --- ROM regions (u32 words, indexed by byte_offset >> 2) ---
    pub maincpu_rom: Vec<u32>,
    pub main_data: Vec<u32>,
    /// Coprocessor data/table ROMs. Immutable after load, so the coprocessor
    /// worker thread reads them through a shared `Arc` instead of a lock.
    pub copro_data: std::sync::Arc<Vec<u32>>,
    pub copro_tables: std::sync::Arc<Vec<u32>>,
    pub polygon_rom: Vec<u32>,
    pub texture_rom: Vec<u16>,
    pub geometry: GeometryEngine,

    // --- Main CPU RAM ---
    /// 0x00200000-0x0021ffff, 128KB scratch (model2o only).
    pub ram_low: Vec<u32>,
    /// 0x00500000-0x005fffff, 1MB work RAM.
    pub work_ram: Vec<u32>,
    /// Write epochs for the RAM the i960 can execute from, indexed by 4 KiB
    /// page (`addr >> 12`), covering ram_low and work RAM (pages 0x200-0x5FF;
    /// all lower pages are ROM, whose writes are dropped). The dynarec's
    /// compiled blocks record the epochs of the pages they span; a write
    /// under a compiled block bumps its page and forces recompilation, which
    /// is how uploaded and self-modifying code stays correct. A flat array
    /// because the check runs on every block dispatch. Runtime-only, never
    /// serialized.
    pub code_epochs: [u64; 0x600],
    /// 0x00900000-0x0091ffff (mirror 0x60000), 128KB geometry buffer.
    /// Dual-ported between the i960 and the coprocessor on the real board,
    /// hence the word-atomic storage: with the coprocessor on its own
    /// thread both ports are live at once.
    pub buffer_ram: crate::copro::SharedBuffer,

    // --- Video ---
    /// segas24_tile: tile_r/tile_w at 0x01000000, 64KB of tilemap entries.
    pub tile_ram: Vec<u32>,
    /// segas24_tile: char_r/char_w at 0x01080000, 512KB of character bitmaps.
    pub char_ram: Vec<u32>,
    pub palette_ram: Vec<u32>,
    pub colorxlat_ram: Vec<u32>,
    /// 32K x 8 luma lookup RAM. The 128KB i960 aperture only wires byte lane 0.
    pub luma_ram: Vec<u8>,
    pub texture_ram0: Vec<u32>,
    pub texture_ram1: Vec<u32>,

    // --- Misc RAM ---
    /// 0x01d00000-0x01d03fff, battery-backed SRAM (NVRAM default is all-ones).
    pub backup_ram: Vec<u32>,
    /// MB8421 dual-port RAM shared with the Model 1 I/O board: 2K x 8, mapped
    /// at 0x01c00000 with umask32(0x00ff00ff), so only byte lanes 0 and 2 of
    /// each word are wired and the device address is `cpu_addr >> 1`.
    pub dpram: Vec<u8>,
    /// 0x00e00000-0x00e00037, CPU wait-state control.
    pub cpu_ctl: Vec<u32>,

    // --- M2COMM network board (837-10537) ---
    /// Whether the board is fitted at all. A standalone cabinet ships without
    /// one, and the game copes: `cn_r` floats to 0xff, bit 0 reads back set
    /// where the board would have driven it low, and the boot code takes its
    /// "no board" branch. See `Cabinet` for why this is the honest way to model
    /// a single cabinet.
    pub comm_present: bool,
    /// The sound board (MultiPCM on original Model 2, SCSP on 2A). Its only
    /// wire to us is the UART at 0x01c80000.
    pub sound: Sound,
    /// Last byte the game sent to the drive board. See `io_service_drive`.
    pub drive_cmd: u8,
    /// 0x01a00000-0x01a03fff, 16KB shared RAM, an 8-bit device on consecutive
    /// byte addresses.
    pub comm_shared: Vec<u8>,
    /// Control register at 0x01a04000, bit 0 enables the board.
    pub comm_cn: u8,
    /// Flag registers at 0x01a04002.
    pub comm_fg: u8,
    pub comm_zfg: u8,
    /// Delays the first `read_fg` poll after a tick, so a frame the board has
    /// only just queued is not reported twice in one game frame.
    pub comm_zfg_delay: u8,
    /// Link state.
    pub comm_linkenable: u8,
    pub comm_linkalive: u8,
    pub comm_linkid: u8,
    pub comm_linkcount: u8,
    pub comm_linktimer: u16,
    /// The reference, off by default (its `comm_framesync` option).
    pub comm_framesync: u16,
    /// The ring itself. The comm boards are wired in a loop: each board's
    /// transmitter feeds the next board's receiver, and the last feeds back
    /// into the first. The reference puts sockets on the two ends; with one cabinet the
    /// loop closes on itself, so a frame sent is a frame received and this
    /// queue *is* the cable.
    pub comm_ring: std::collections::VecDeque<Vec<u8>>,

    // --- TGP (MB86234) memory ---
    /// copro_tgp_prog_map: 0x000-0xfff, 4096 words.
    pub tgp_program_ram: Vec<u32>,
    /// copro_tgp_data_map: 0x0000-0x00ff and 0x0200-0x03ff.
    pub tgp_data_ram: Vec<u32>,

    // --- FIFOs ---
    pub copro_fifo_in: std::collections::VecDeque<u32>,
    pub copro_fifo_out: std::collections::VecDeque<u32>,

    // --- Registers ---
    pub irq_request: u32,
    pub irq_enable: u32,

    /// Coprocessor control (0x00980000). Bit 31 = microcode upload in progress.
    pub copro_ctl: u32,
    /// Write pointer for the microcode upload triggered by `copro_ctl` bit 31.
    pub copro_cnt: u32,
    pub copro_halted: bool,
    /// Bank register written through TGP register-file port 3.
    pub copro_bank_reg: u32,
    pub copro_sincos_base: u32,
    pub copro_atan_base: [u32; 4],
    /// TGP GPIO0: the atan comparator's |a| <= |b| line. Board state, not CPU
    /// state -- see the atan write handler in memory.rs.
    pub copro_gpio0: bool,
    pub copro_inv_base: u32,
    pub copro_isqrt_base: u32,
    /// Set when a TGP FIFO port could not service a read; makes the TGP retry
    /// the instruction instead of consuming a bogus value.
    pub copro_stall: bool,
    /// Set when the i960 reads an empty coprocessor output FIFO. The load is
    /// retried after the TGP produces a word.
    pub main_stall: bool,

    /// Geometrizer control (0x00980008). Bit 31 = microcode upload in progress.
    pub geo_ctl: u32,
    pub geo_cnt: u32,
    pub geo_write_start_address: u32,
    pub geo_read_start_address: u32,

    /// Set once the game programs the colour-translation RAM, so the renderer
    /// knows whether it can trust it.
    pub colorxlat_written: bool,
    /// Set when the colour translation RAM changes, so the monitor transfer can
    /// be re-derived at the next V-blank rather than on every write.
    pub colorxlat_dirty: bool,
    /// Transfer curve of the cabinet's monitor, indexed by the 8-bit signal the
    /// colour translation RAM hands to the video DAC. See `rebuild_monitor`.
    pub monitor: [u8; 256],
    pub crtc_xoffset: i16,
    pub crtc_yoffset: i16,
    pub crtc_xraw: u16,
    pub crtc_yraw: u16,

    /// Video control (0x0098000c).
    pub video_ctl: u32,
    /// Rasterizer mode register (0x10000000): bits 14, 2 and 0 are readable.
    pub render_mode_ctl: u32,
    pub frame_num: u32,

    // Timer registers (0x00f00000-0x00f0000f)
    pub timer_vals: [u32; 4],
    /// Size of the quantum currently being executed, so a timer read can work
    /// out how far into it the CPU has got.
    pub quantum_cycles: i32,
    /// Free-running 25 MHz machine clock, the time base the hardware timers
    /// are measured against.
    pub machine_cycles: u64,
    /// Value each timer was programmed with, and the machine time at which it
    /// was.
    pub timer_orig: [u32; 4],
    pub timer_start: [u64; 4],
    pub timer_running: [bool; 4],

    /// Machine configuration (cabinet mode, ROM path).
    pub config: Config,
    /// Live control state fed to the I/O board HLE.
    pub inputs: Inputs,

    /// Logs dual-port RAM traffic, for reverse-engineering the I/O protocol.
    /// Short name of the loaded game, stamped into save states.
    /// Interrupt line states waiting to be handed to the running CPU.
    pub pending_irq_lines: Option<[bool; 4]>,
    /// Last state of the UART ready line, so bit 10 is asserted on its edge.
    pub sound_ready_line: bool,
    /// Interrupt mask waiting to take effect, with the machine time it lands.
    /// The reference delays this by 80ns (two 25 MHz cycles) -- an interrupt dispatched
    /// in that window still sees the previous mask, which is what keeps a
    /// handler from deciding its own source was spurious.
    pub irq_enable_pending: Option<(u32, u64)>,
    pub snapshot_game: String,
}

impl Model2System {
    pub fn new(roms: &Roms) -> Self {
        Self::with_config(roms, Config::default())
    }

    pub fn with_config(roms: &Roms, config: Config) -> Self {
        fn to_words(bytes: &[u8]) -> Vec<u32> {
            bytes
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }

        let mut system = Self {
            main_cpu: Box::new(I960Cpu::new()),
            tgp_cpu: Box::new(Mb86233::new()),
            sharc: Box::new(sharc::Sharc::new()),
            tgpx4: Box::new(mb86235::Mb86235::new()),
            coprocessor: roms.coprocessor,
            sharc_reads: 0,
            sharc_writes: 0,
            sharc_read_addrs: [0; 4],
            sharc_write_addrs: [0; 4],
            sharc_write_samples: [0; 8],
            io5649_ports: [0xff; 8],
            io5649_analog: 0,
            io5649_gun_mux: 0,
            eeprom: {
                // A romset that ships a factory EEPROM is one whose cabinet
                // configuration the game cannot reach from its own menus, so
                // start from that image rather than a blank chip.
                let mut e = crate::eeprom93c46::Eeprom93c46::new();
                if !roms.eeprom.is_empty() {
                    for (i, w) in e.data.iter_mut().enumerate() {
                        if let (Some(lo), Some(hi)) =
                            (roms.eeprom.get(i * 2), roms.eeprom.get(i * 2 + 1))
                        {
                            *w = u16::from_le_bytes([*lo, *hi]);
                        }
                    }
                    log::info!(target: "nvram", "seeded EEPROM from the romset's factory image");
                }
                e
            },
            io5649_ctrlmode: false,
            tgpx4_pops: 0,
            tgpx4_pushes: 0,
            tgpx4_ext_r: 0,
            tgpx4_ext_w: 0,
            tgpx4_rbucket: [0; 3],
            tgpx4_rsample: [0; 8],
            geo_pushes: 0,
            parked_main: Some(Box::new(I960Cpu::new())),
            parked_tgp: Some(Box::new(Mb86233::new())),
            parked_sharc: Some(Box::new(sharc::Sharc::new())),
            parked_tgpx4: Some(Box::new(mb86235::Mb86235::new())),
            copro_mt: None,

            maincpu_rom: to_words(&roms.maincpu),
            main_data: to_words(&roms.main_data),
            copro_data: std::sync::Arc::new(to_words(&roms.copro_data)),
            copro_tables: std::sync::Arc::new(to_words(&roms.copro_tables)),
            polygon_rom: to_words(&roms.polygons),
            texture_rom: roms
                .textures
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect(),
            geometry: GeometryEngine::default(),

            ram_low: vec![0; 0x20000 / 4],
            work_ram: vec![0; 0x100000 / 4],
            code_epochs: [0; 0x600],
            // The reference initializes the geometry buffer to the hardware's benign
            // end-code pattern, not zero. This matters before the first full
            // display list has overwritten both halves.
            buffer_ram: crate::copro::SharedBuffer::new(0x0780_0f0f, 0x20000 / 4),

            tile_ram: vec![0; 0x10000 / 4],
            char_ram: vec![0; 0x80000 / 4],
            palette_ram: vec![0; 0x4000 / 4],
            colorxlat_ram: vec![0; 0xc000 / 4],
            luma_ram: vec![0; 0x8000],
            // The i960 aperture is 2MB, but each bus write contributes only
            // its low 16 bits. Adjacent aperture dwords are packed into one
            // 32-bit rasterizer word.
            // 2MB each: the reference maps 0x11000000/0x11200000 as plain 2MB RAM on
            // the 2B/2C video board. The original/2A boards reach the same
            // memory through a 2MB *aperture* onto its low half.
            texture_ram0: vec![0; 0x200000 / 4],
            texture_ram1: vec![0; 0x200000 / 4],

            // NVRAM(config, "backup1", nvram_device::DEFAULT_ALL_1)
            backup_ram: vec![0xFFFF_FFFF; 0x4000 / 4],
            dpram: vec![0; 0x800],
            cpu_ctl: vec![0; 0x40 / 4],

            comm_present: config.cabinet == Cabinet::Twin,
            sound: if roms.sound_scsp {
                // Model 2A: the two 4MB halves concatenate into the SCSP
                // board's 8MB "samples" region.
                let mut samples = roms.mpcm1.clone();
                samples.extend_from_slice(&roms.mpcm2);
                Sound::Scsp(Box::new(crate::sound2a::SoundSystem2A::new(
                    roms.sndcpu.clone(),
                    samples,
                )))
            } else {
                Sound::MultiPcm(Box::new(SoundSystem::new(
                    roms.sndcpu.clone(),
                    roms.mpcm1.clone(),
                    roms.mpcm2.clone(),
                )))
            },
            drive_cmd: 0,
            comm_shared: vec![0; 0x4000],
            comm_cn: 0,
            comm_fg: 0,
            comm_zfg: 0,
            comm_zfg_delay: 0,
            comm_linkenable: 0,
            comm_linkalive: 0,
            comm_linkid: 0,
            comm_linkcount: 0,
            comm_linktimer: 0,
            comm_framesync: 0,
            comm_ring: std::collections::VecDeque::new(),

            tgp_program_ram: vec![0; 0x1000],
            tgp_data_ram: vec![0; 0x400],

            copro_fifo_in: std::collections::VecDeque::new(),
            copro_fifo_out: std::collections::VecDeque::new(),

            irq_request: 0,
            irq_enable: 0,

            copro_ctl: 0,
            copro_cnt: 0,
            // The TGP idles until the main CPU uploads microcode and boots it.
            copro_halted: true,
            copro_bank_reg: 0,
            copro_sincos_base: 0,
            copro_atan_base: [0; 4],
            copro_gpio0: false,
            copro_inv_base: 0,
            copro_isqrt_base: 0,
            copro_stall: false,
            main_stall: false,

            geo_ctl: 0,
            geo_cnt: 0,
            geo_write_start_address: 0,
            geo_read_start_address: 0,

            colorxlat_written: false,
            colorxlat_dirty: false,
            // Identity until the game programs its curve.
            monitor: std::array::from_fn(|i| i as u8),
            crtc_xoffset: 90,
            crtc_yoffset: -8,
            crtc_xraw: 6,
            crtc_yraw: (-138i16) as u16,
            video_ctl: 0,
            render_mode_ctl: 0,
            frame_num: 0,

            // The reference starts every timer at its maximum, and
            // an idle timer reads back that value. Virtua Fighter 2 seeds a
            // random number generator by summing all four, so starting them at
            // zero leaves the low bits of the seed constant and its shuffle
            // never finds an unused slot.
            timer_vals: [0xfffff; 4],
            quantum_cycles: 0,
            machine_cycles: 0,
            timer_orig: [0xfffff; 4],
            timer_start: [0; 4],
            timer_running: [false; 4],

            config,
            inputs: Inputs::default(),

            pending_irq_lines: None,
            sound_ready_line: false,
            irq_enable_pending: None,
            snapshot_game: String::new(),
        };

        // The I/O board reports itself ready before the CPU issues commands.
        system.dpram[IO_STATUS] = 0x40;

        log::info!(target: "system", "Resetting i960 CPU...");
        let mut cpu = std::mem::replace(&mut system.main_cpu, Box::new(I960Cpu::new()));
        cpu.reset(&mut system);
        system.main_cpu = cpu;

        system.sharc.jit_enabled = system.config.sharc_jit;
        system.tgpx4.jit_enabled = system.config.mb86235_jit;
        if system.config.multithreaded {
            log::info!(target: "copro", "geometry coprocessor on worker thread");
            system.start_copro_worker();
        }

        system
    }

    /// Mirrors the reference: a change to bit 31 starts a microcode
    /// upload (halting the TGP) or boots the TGP with what was uploaded.
    /// Whether this board's geometry coprocessor is the ADSP-21062 SHARC (2B)
    /// rather than the MB86234 TGP.
    /// Whether this board carries the 315-5649 I/O chip (2A/2B/2C) rather than
    /// the Model 1 I/O board behind a dual-port RAM (original Model 2).
    #[inline]
    pub fn has_io5649(&self) -> bool {
        !matches!(self.coprocessor, crate::roms_db::Board::Model2o)
    }

    /// Whether this board's geometry coprocessor is the MB86235 TGPx4 (2C).
    #[inline]
    pub fn is_tgpx4(&self) -> bool {
        self.coprocessor == crate::roms_db::Board::Model2c
    }

    #[inline]
    pub fn is_sharc(&self) -> bool {
        self.coprocessor == crate::roms_db::Board::Model2b
    }

    pub fn copro_ctl_w(&mut self, val: u32) {
        if (val ^ self.copro_ctl) == 0x8000_0000 {
            if val & 0x8000_0000 != 0 {
                log::info!(target: "copro", "Start microcode upload");
                self.copro_cnt = 0;
                self.copro_halted = true;
                if let Some(mt) = &self.copro_mt {
                    mt.set_halted(true);
                    if self.is_tgpx4() {
                        mt.control(crate::copro::ControlOp::Tgpx4Upload {
                            index: self.copro_cnt,
                            value: val,
                        });
                    } else if self.is_sharc() {
                        mt.control(crate::copro::ControlOp::SharcReset);
                    }
                } else if self.is_tgpx4() {
                    let cnt = self.copro_cnt;
                    self.tgpx4.upload_program_half(cnt, val);
                } else if self.is_sharc() {
                    self.sharc.reset();
                }
            } else if self.is_tgpx4() {
                log::info!(target: "copro", "Boot TGPx4, {} words uploaded", self.copro_cnt);
                self.copro_halted = false;
                if let Some(mt) = &self.copro_mt {
                    mt.control(crate::copro::ControlOp::Tgpx4Reset);
                    mt.set_halted(false);
                } else {
                    self.tgpx4.reset();
                }
            } else if self.is_sharc() {
                log::info!(target: "copro", "Boot SHARC, {} words uploaded", self.copro_cnt);
                self.copro_halted = false;
                if let Some(mt) = &self.copro_mt {
                    mt.control(crate::copro::ControlOp::SharcBoot);
                    mt.set_halted(false);
                } else {
                    // The SHARC begins executing the just-uploaded program at the
                    // start of internal RAM.
                    self.sharc.pc = 0x20004;
                    self.sharc.daddr = 0x20004;
                    self.sharc.faddr = 0x20005;
                    self.sharc.nfaddr = 0x20006;
                    self.sharc.idle = false;
                }
            } else {
                log::info!(target: "copro", "Boot TGP, {} dwords uploaded", self.copro_cnt);
                self.copro_halted = false;
                if let Some(mt) = &self.copro_mt {
                    mt.control(crate::copro::ControlOp::TgpReset);
                    mt.set_halted(false);
                } else {
                    self.tgp_cpu.reset();
                }
            }
        }
        self.copro_ctl = val;
    }

    /// The geometrizer's control register: reset, halt, and the bank the
    /// display list is written into.
    pub fn geo_ctl_w(&mut self, val: u32) {
        if (val ^ self.geo_ctl) == 0x8000_0000 {
            if val & 0x8000_0000 != 0 {
                log::info!(target: "geo", "Start geometrizer upload");
                self.geo_cnt = 0;
            } else {
                log::info!(target: "geo", "Boot geometrizer, {} dwords uploaded", self.geo_cnt);
            }
        }
        self.geo_ctl = val;
    }

    /// Mirrors the reference (0x00884000): while bit 31 of copro_ctl is
    /// set the writes are microcode words, not FIFO traffic. Without this the
    /// TGP executes an empty program RAM.
    pub fn copro_fifo_w(&mut self, val: u32) {
        if self.copro_ctl & 0x8000_0000 != 0 {
            if self.is_tgpx4() {
                let cnt = self.copro_cnt;
                if let Some(mt) = &self.copro_mt {
                    mt.control(crate::copro::ControlOp::Tgpx4Upload {
                        index: cnt,
                        value: val,
                    });
                } else {
                    self.tgpx4.upload_program_half(cnt, val);
                }
            } else if self.is_sharc() {
                // The SHARC boots from the host: each 16-bit word is fed to its
                // DMA controller, which packs three into one 48-bit program
                // word.
                let cnt = self.copro_cnt;
                if let Some(mt) = &self.copro_mt {
                    mt.control(crate::copro::ControlOp::SharcDma {
                        index: cnt,
                        value: val,
                    });
                } else {
                    let mut sharc = std::mem::replace(
                        &mut self.sharc,
                        self.parked_sharc.take().expect("sharc placeholder"),
                    );
                    sharc.external_dma_write(self, cnt, val & 0xffff);
                    self.parked_sharc = Some(std::mem::replace(&mut self.sharc, sharc));
                }
            } else {
                let idx = self.copro_cnt as usize;
                if let Some(mt) = &self.copro_mt {
                    mt.write_tgp_program(self.copro_cnt, val);
                } else if idx < self.tgp_program_ram.len() {
                    self.tgp_program_ram[idx] = val;
                }
            }
            self.copro_cnt += 1;
        } else {
            // The reference the FIFO accepts one or more overflow values before
            // its zero-time synchronization callback halts the producer. In
            // our instruction-interleaved scheduler that is exactly one word:
            // accept the ninth word, then run_slice stops the i960 until the
            // TGP has consumed the overflow entry.
            self.copro_fifo_in_push(val);
        }
    }

    /// Appends one word to the display list and advances the write pointer.
    pub fn push_geo_data(&mut self, val: u32) {
        self.geo_pushes += 1;
        let idx = self.geo_write_start_address >> 2;
        if (idx as usize) < self.buffer_ram.len() {
            self.buffer_ram.write(idx, val);
        }
        self.geo_write_start_address = self.geo_write_start_address.wrapping_add(4);
    }

    // --- Coprocessor FIFO access ---
    //
    // With the coprocessor on its worker thread the authoritative FIFOs live
    // in the shared block; the system's own `copro_fifo_*` are only the
    // single-threaded path's copies (and the savestate staging area).

    pub(crate) fn copro_fifo_in_push(&mut self, val: u32) {
        match &self.copro_mt {
            Some(mt) => mt.push_input(val),
            None => self.copro_fifo_in.push_back(val),
        }
    }

    pub(crate) fn copro_fifo_out_pop(&mut self) -> Option<u32> {
        match &self.copro_mt {
            Some(mt) => mt.pop_output(),
            None => self.copro_fifo_out.pop_front(),
        }
    }

    pub(crate) fn copro_fifo_out_is_empty(&self) -> bool {
        match &self.copro_mt {
            Some(mt) => mt.output_empty(),
            None => self.copro_fifo_out.is_empty(),
        }
    }

    /// Input FIFO depth, for `run_slice`'s producer-overflow check.
    fn copro_fifo_in_len(&self) -> usize {
        match &self.copro_mt {
            Some(mt) => mt.input_len(),
            None => self.copro_fifo_in.len(),
        }
    }

    /// Mirrors the reference: fan the 12 interrupt request bits out onto
    /// the i960's four IRQ lines.
    pub fn irq_update(&mut self) {
        // The reference only ever sets a request bit whose mask is already enabled, so
        // its `intreq` never holds a masked interrupt. Ours can, because the
        // game masks a line it has already requested -- so gate the CPU lines
        // with the mask here instead. Dispatching a masked interrupt makes the
        // game's handler decide it is spurious and halt.
        let req = self.irq_request & self.irq_enable;
        let lines = [
            req & 0b0000_0000_0001 != 0,
            req & 0b0000_0000_0010 != 0,
            req & 0b0011_1111_1100 != 0,
            req & 0b1100_0000_0000 != 0,
        ];
        // While the i960 is running it has been moved out of this struct, so
        // `self.main_cpu` is a parked placeholder and poking it would be lost.
        // Hand the new line states to the core through the bus instead; it
        // applies them before its next instruction.
        self.pending_irq_lines = Some(lines);
    }

    /// The reference: the UART's TxRDY/RxRDY lines drive interrupt
    /// bit 10, so it is re-asserted the moment either goes ready -- including
    /// synchronously from the handler's own write to the sound port, which is
    /// how the game's sound task re-enters itself.
    /// Applies a delayed interrupt-mask write once its two cycles have passed.
    /// The reference re-evaluates the sound-ready line at the same moment
    /// (`irq_mask_delayed_update`).
    pub fn irq_enable_tick(&mut self) {
        if let Some((val, at)) = self.irq_enable_pending {
            if self.now_cycles() >= at {
                self.irq_enable_pending = None;
                self.irq_enable = val;
                self.irq_update();
                let ready = self.sound.tx_ready() || self.sound.reply_ready();
                if ready && self.irq_enable & (1 << 10) != 0 {
                    self.irq_request |= 1 << 10;
                    self.irq_update();
                }
            }
        }
    }

    pub fn sound_ready_update(&mut self) {
        // The reference drives this from the UART's TxRDY/RxRDY *handlers*, so the
        // interrupt is asserted when a line goes ready -- not continuously
        // while it is. Re-asserting every quantum floods the game's sound task
        // and it eventually re-enters with the line masked, which Virtua
        // Fighter 2 reports as "interrupt halt".
        let ready = self.sound.tx_ready() || self.sound.reply_ready();
        if ready && !self.sound_ready_line {
            self.raise_irq(10);
        }
        self.sound_ready_line = ready;
    }

    /// Raise interrupt `line` (a bit index) if it is enabled.
    pub fn raise_irq(&mut self, line: u32) {
        let mask = 1 << line;
        if self.irq_enable & mask != 0 {
            self.irq_request |= mask;
            self.irq_update();
        }
    }

    pub fn run_slice(&mut self, cycles: i32) {
        // The two processors exchange traffic through FIFOs only eight words
        // deep. Running one for an entire video frame before the other lets
        // both outbound queues fill and creates an artificial deadlock. The reference
        // scheduler time-slices them; do the same here. The reference configures a
        // maximum quantum of 18 kHz, but FIFO reads/writes are scheduler
        // synchronization points in the reference: the waiting CPU resumes as soon as
        // its peer changes the queue. We do not have an event scheduler, so
        // a 1,389-cycle slice adds up to 55.6us of fictitious latency to every
        // FIFO handshake. Daytona performs enough of them per frame for the
        // simulation to run visibly slow while V-blank still reports 57.5Hz.
        // Use a fine lockstep here; 64 i960 clocks preserve the 3:2 effective
        // clock ratio (25MHz versus 50MHz/3) while bounding that error to
        // 2.56us.
        const MAIN_QUANTUM: i32 = 64;
        const TGP_QUANTUM: i32 = 43;
        // With the coprocessor on its worker thread the DSP block below is
        // skipped entirely: the worker free-runs in batches and the FIFOs
        // alone pace the two sides. The i960's own stall rules -- skipped
        // while the input FIFO overflows, quantum cut short by an empty
        // output read -- are unchanged.
        let mt = self.copro_mt.is_some();
        let mut remaining = cycles;
        while remaining > 0 {
            let step = remaining.min(MAIN_QUANTUM);

            // An overflow word has already been accepted. The producer is
            // halted after that instruction and resumes when the consumer
            // brings the queue back to its configured depth.
            if self.copro_fifo_in_len() <= COPRO_FIFO_DEPTH {
                let parked = self.parked_main.take().expect("i960 placeholder");
                let mut main_cpu = std::mem::replace(&mut self.main_cpu, parked);
                self.quantum_cycles = step;
                // The dynarec is the default execution engine; the
                // interpreter stays as the runtime fallback (`--i960-jit
                // off`) and as the JIT's own per-instruction fallback.
                if self.config.i960_jit {
                    main_cpu.execute_run_jit(self, step);
                } else {
                    main_cpu.execute_run(self, step);
                }
                self.machine_cycles = self.machine_cycles.wrapping_add(step as u64);
                self.parked_main = Some(std::mem::replace(&mut self.main_cpu, main_cpu));
                // MMIO writes made by execute_run mutate the board while the
                // active CPU is temporarily outside `self`. Reconcile IRQ
                // levels only after putting that CPU back; doing it inside
                // irq_ack_w updates the temporary placeholder and loses the
                // falling edge needed for the next V-blank.
                self.irq_update();
            }

            let output_overflowed = self.copro_fifo_out.len() > COPRO_FIFO_DEPTH;
            if !mt && !self.copro_halted && !output_overflowed {
                let cop_step = if step == MAIN_QUANTUM {
                    TGP_QUANTUM
                } else {
                    ((step as i64 * TGP_QUANTUM as i64) / MAIN_QUANTUM as i64).max(1) as i32
                };
                if self.is_tgpx4() {
                    let parked = self.parked_tgpx4.take().expect("tgpx4 placeholder");
                    let mut cop = std::mem::replace(&mut self.tgpx4, parked);
                    cop.execute(self, cop_step);
                    self.parked_tgpx4 = Some(std::mem::replace(&mut self.tgpx4, cop));
                } else if self.is_sharc() {
                    let parked = self.parked_sharc.take().expect("sharc placeholder");
                    let mut sharc = std::mem::replace(&mut self.sharc, parked);
                    sharc.execute(self, cop_step);
                    self.parked_sharc = Some(std::mem::replace(&mut self.sharc, sharc));
                } else {
                    let parked = self.parked_tgp.take().expect("tgp placeholder");
                    let mut tgp = std::mem::replace(&mut self.tgp_cpu, parked);
                    tgp.execute(self, cop_step);
                    self.parked_tgp = Some(std::mem::replace(&mut self.tgp_cpu, tgp));
                }
            }

            // The four hardware timers run from the 25 MHz i960 clock and
            // assert their IRQ at the instant they expire. Advancing them
            // only once after a whole video frame delayed an IRQ by as much
            // as 17.4 ms, which perturbed Daytona's replay/physics and camera
            // state. The scheduler quantum bounds the remaining error to
            // roughly 56 us while preserving the real clock domain.
            for i in 0..4 {
                if self.timer_running[i] {
                    if self.timer_vals[i] > step as u32 {
                        self.timer_vals[i] -= step as u32;
                    } else {
                        self.timer_vals[i] = 0xfffff;
                        self.timer_running[i] = false;
                        self.raise_irq(i as u32 + 2);
                    }
                }
            }

            // The sound board runs from its own 10MHz crystal; give it the
            // matching slice of time: a 20 MHz crystal divided by two.
            self.sound.run(step, 25_000_000);
            self.irq_enable_tick();

            // The reference: interrupt bit 10 is asserted while
            // *either* UART direction is ready. It is driven by the UART's
            // ready lines, so it lands partway through a frame -- checking it
            // once per frame instead delays the game's sound task by up to a
            // whole frame, and Virtua Fighter 2's boot sequence diverges on
            // exactly that timing.
            if self.sound.tx_ready() || self.sound.reply_ready() {
                self.raise_irq(10);
            }

            remaining -= step;
        }
    }

    /// The battery-backed state worth keeping between runs: the backup SRAM
    /// and the serial EEPROM.
    pub fn nvram_blocks(&self) -> (Vec<u8>, Vec<u8>) {
        let mut backup = Vec::with_capacity(self.backup_ram.len() * 4);
        for w in &self.backup_ram {
            backup.extend_from_slice(&w.to_le_bytes());
        }
        let mut eeprom = Vec::with_capacity(self.eeprom.data.len() * 2);
        for w in &self.eeprom.data {
            eeprom.extend_from_slice(&w.to_le_bytes());
        }
        (backup, eeprom)
    }

    /// Restores what `nvram_blocks` produced. Sizes are checked by the caller.
    pub fn set_nvram_blocks(&mut self, backup: &[u8], eeprom: &[u8]) {
        for (w, c) in self.backup_ram.iter_mut().zip(backup.chunks_exact(4)) {
            *w = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        }
        for (w, c) in self.eeprom.data.iter_mut().zip(eeprom.chunks_exact(2)) {
            *w = u16::from_le_bytes([c[0], c[1]]);
        }
    }

    pub fn nvram_sizes(&self) -> (usize, usize) {
        (self.backup_ram.len() * 4, self.eeprom.data.len() * 2)
    }

    /// The machine clock as of this instant, including how far the CPU has got
    /// into the quantum it is running. Timer reads land mid-quantum and games
    /// use their low bits as entropy, so that part matters.
    /// Executes a single i960 instruction, for the debugger's stepper.
    pub fn step_instruction(&mut self) {
        let parked = self.parked_main.take().expect("i960 placeholder");
        let mut main_cpu = std::mem::replace(&mut self.main_cpu, parked);
        self.quantum_cycles = 1;
        main_cpu.execute_run(self, 1);
        self.parked_main = Some(std::mem::replace(&mut self.main_cpu, main_cpu));
        self.machine_cycles = self.machine_cycles.wrapping_add(1);
        self.irq_update();
    }

    pub fn now_cycles(&self) -> u64 {
        let remaining = i960::cpu::core::LIVE_ICOUNT.load(std::sync::atomic::Ordering::Relaxed);
        self.machine_cycles + (self.quantum_cycles - remaining).max(0) as u64
    }

    fn nv_byte(&self, off: usize) -> u8 {
        (self.backup_ram[off >> 2] >> ((off & 3) * 8)) as u8
    }

    fn nv_set_byte(&mut self, off: usize, v: u8) {
        let sh = (off & 3) * 8;
        let w = &mut self.backup_ram[off >> 2];
        *w = (*w & !(0xFFu32 << sh)) | ((v as u32) << sh);
    }

    /// The game's settings checksum: a stock CRC-16/CCITT (poly 0x1021, MSB
    /// first, zero seed) over `base+0x0a.. base+0x80`. The boot code drives it
    /// off a lookup table in ROM, which is bit-for-bit the standard one.
    fn nv_crc(&self, base: usize) -> u16 {
        let mut crc: u16 = 0;
        for off in (base + NV_DATA)..(base + NV_END) {
            let idx = ((crc >> 8) as u8) ^ self.nv_byte(off);
            crc = (crc << 8) ^ CRC16_CCITT[idx as usize];
        }
        crc
    }

    fn nv_block_sealed(&self, base: usize) -> bool {
        let stored =
            self.nv_byte(base + NV_CRC) as u16 | ((self.nv_byte(base + NV_CRC + 1) as u16) << 8);
        stored == self.nv_crc(base)
    }

    fn nv_seal(&mut self, base: usize) {
        let crc = self.nv_crc(base);
        self.nv_set_byte(base + NV_CRC, crc as u8);
        self.nv_set_byte(base + NV_CRC + 1, (crc >> 8) as u8);
    }

    /// Applies the configured cabinet to the settings, the way the test menu
    /// does when an operator changes it: set the role byte, then re-seal the
    /// block so the boot code still trusts it.
    ///
    /// The game rebuilds its working block from ROM defaults on every boot
    /// where the saved copy does not validate, sealing it as the last step, so
    /// this is driven off the seal rather than run once at reset -- otherwise
    /// the rebuild would simply overwrite it.
    pub fn nv_apply_cabinet(&mut self) {
        let want = self.config.cabinet.link_role();
        for base in [0usize, NV_SAVED] {
            if !self.nv_block_sealed(base) || self.nv_byte(base + NV_ROLE) == want {
                continue;
            }
            self.nv_set_byte(base + NV_ROLE, want);
            self.nv_seal(base);
        }
    }

    /// Geometry of one ring frame, read back out of the shared RAM the board
    /// published at reset. `frameStart` is fixed in the board's firmware.
    fn comm_frame_geometry(&self) -> (usize, usize, usize) {
        let frame_start = 0x2000usize;
        let frame_size = ((self.comm_shared[0x13] as usize) << 8) | self.comm_shared[0x12] as usize;
        let frame_offset = frame_start
            | ((self.comm_shared[0x15] as usize) << 8)
            | self.comm_shared[0x14] as usize;
        (frame_start, frame_size, frame_offset)
    }

    /// The node's role, which the game selects through `fg` and `shared[1]`.
    /// EPR-16726 (Daytona's comm ROM) uses `fg`.
    fn comm_roles(&self) -> (bool, bool, bool) {
        let is_master = self.comm_fg == 0x01 || self.comm_shared[1] == 0x01;
        let is_slave = self.comm_fg == 0x00 && self.comm_shared[1] == 0x02;
        let is_relay = self.comm_fg == 0x00 && self.comm_shared[1] == 0x00;
        (is_master, is_slave, is_relay)
    }

    fn comm_read_frame(&mut self) -> Option<Vec<u8>> {
        self.comm_ring.pop_front()
    }

    fn comm_send_frame(&mut self, frame: Vec<u8>) {
        self.comm_ring.push_back(frame);
    }

    /// Sends this node's slice of the ring buffer, tagged with `frame_type`.
    fn comm_send_data(&mut self, frame_type: u8) {
        let (frame_start, frame_size, _) = self.comm_frame_geometry();
        let mut frame = Vec::with_capacity(frame_size + 1);
        frame.push(frame_type);
        frame.extend_from_slice(&self.comm_shared[frame_start..frame_start + frame_size]);
        self.comm_send_frame(frame);
    }

    /// Bit 0 enables the board; clearing
    /// it resets. Enabling zeroes the shared RAM and publishes the board's
    /// parameters, which is how the game learns the frame layout.
    pub fn comm_cn_w(&mut self, data: u8) {
        self.comm_cn = data & 0x01;
        if self.comm_cn == 0 {
            self.comm_linkenable = 0;
            self.comm_zfg = 0;
            self.comm_fg = 0;
            self.comm_ring.clear();
            return;
        }

        self.comm_linkenable = 0x01;
        self.comm_linkid = 0x00;
        self.comm_linkalive = 0x00;
        self.comm_linkcount = 0x00;
        self.comm_linktimer = 0x00e8; // ~58fps * 4s
        self.comm_ring.clear();

        self.comm_shared.iter_mut().for_each(|b| *b = 0);
        self.comm_shared[0x01] = 0x02;
        // frameSize 0x0e00
        self.comm_shared[0x12] = 0x00;
        self.comm_shared[0x13] = 0x0e;
        // frameOffset 0x01c0
        self.comm_shared[0x14] = (COMM_FRAME_OFFSET & 0xff) as u8;
        self.comm_shared[0x15] = (COMM_FRAME_OFFSET >> 8) as u8;

        self.comm_tick();
    }

    /// Consumes every frame currently on the ring, which is what both
    /// `comm_tick` and `read_fg` do once the link is up. Each accepted data
    /// frame lands in the ring buffer and toggles `zfg`: that toggle is the
    /// board's "a frame came round" signal, and the game's network check spins
    /// until it sees it.
    fn comm_drain_ring(&mut self) {
        let (_, frame_size, frame_offset) = self.comm_frame_geometry();
        let (is_master, is_slave, is_relay) = self.comm_roles();

        while let Some(frame) = self.comm_read_frame() {
            let idx = frame[0];
            if idx <= self.comm_linkcount && frame.len() > frame_size {
                self.comm_shared[frame_offset..frame_offset + frame_size]
                    .copy_from_slice(&frame[1..1 + frame_size]);
                self.comm_zfg ^= 0x01;
                if is_slave {
                    self.comm_send_data(self.comm_linkid);
                } else if is_relay {
                    self.comm_send_frame(frame);
                }
            } else if idx == 0xfc {
                // V-sync marker: the master drops it, everyone else passes it on.
                self.comm_linktimer = 0x00;
                if !is_master {
                    self.comm_send_frame(frame);
                }
            }
        }
    }

    /// Per-frame link service, called from V-blank.
    pub fn comm_tick(&mut self) {
        if self.comm_linkenable != 0x01 {
            return;
        }

        let (is_master, ..) = self.comm_roles();

        if self.comm_linkalive == 0x02 {
            // Link lost.
            self.comm_shared[0] = 0xff;
            return;
        }

        if self.comm_linkalive == 0x00 {
            // Link not yet established.
            self.comm_shared[0] = 0x00;
            self.comm_shared[2] = 0xff;
            self.comm_shared[3] = 0xff;

            // The ring is intact (it always is here: it closes on this board),
            // so service it. The reference gates this on both sockets being open, which
            // is the same test expressed in terms of its transport.
            self.comm_zfg ^= 0x01;

            while let Some(frame) = self.comm_read_frame() {
                let idx = frame[0];
                if idx == 0xff {
                    // Link id assignment coming back round the ring.
                    if is_master {
                        // The master takes the first id and moves on.
                        self.comm_linkid = 0x01;
                        self.comm_linkcount = frame[1];
                        self.comm_linktimer = 0x00;
                    } else {
                        let mut frame = frame;
                        if self.comm_roles().1 {
                            frame[1] += 1;
                        }
                        self.comm_send_frame(frame);
                    }
                } else if idx == 0xfe {
                    // Final node count.
                    if !is_master {
                        self.comm_linkcount = frame[1];
                        self.comm_linkalive = 0x01;
                        self.comm_send_frame(frame);
                    }
                }
            }

            if is_master && self.comm_linkalive == 0x00 {
                if self.comm_linktimer == 0x01 {
                    // Probe the ring: ask every node downstream to count itself.
                    self.comm_send_frame(vec![0xff, 0x01, 0x00]);
                } else if self.comm_linktimer == 0x00 {
                    // Publish the final count and consider the link up.
                    let n = self.comm_linkcount;
                    self.comm_send_frame(vec![0xfe, n, n]);
                    self.comm_linkalive = 0x01;
                    self.comm_shared[0] = 0x01;
                    self.comm_shared[2] = self.comm_linkid;
                    self.comm_shared[3] = self.comm_linkcount;
                } else {
                    self.comm_linktimer -= 1;
                }
            }
        }

        if self.comm_linkalive == 0x01 {
            self.comm_drain_ring();

            self.comm_linktimer = self.comm_framesync;

            if is_master {
                // Put this node's slice back on the ring, then mark the frame.
                self.comm_send_data(self.comm_linkid);
                self.comm_send_frame(vec![0xfc, 0x01]);
            }

            self.comm_zfg_delay = 0x02;
        }
    }

    /// Mirrors the reference: polling the flag register lets the board catch
    /// up on any frames that arrived since the last V-blank.
    pub fn comm_read_fg(&mut self) {
        if self.comm_zfg_delay > 0 {
            self.comm_zfg_delay -= 1;
            return;
        }
        if self.comm_linkalive == 0x01 {
            self.comm_drain_ring();
        }
    }

    /// Executes one I/O board command. The board acknowledges by zeroing
    /// `IO_CMD`; the caller does that.
    pub fn io_board_command(&mut self, cmd: u8) {
        log::info!(target: "io", "command {:02X}", cmd);
        // Command 1 is issued every frame: the board samples the controls and
        // publishes them for the main CPU.
        if cmd == 0x01 {
            self.io_publish_inputs();
            self.io_service_drive();
        }
    }

    /// Writes the current control state into the dual-port RAM the way the I/O
    /// board's Z80 would.
    fn io_publish_inputs(&mut self) {
        let i = self.inputs;
        for (off, v) in [
            (IO_STEER, i.steer),
            (IO_ACCEL, i.accel),
            (IO_BRAKE, i.brake),
            (IO_ANALOG3, 0xFF),
            (IO_IN0, i.in0),
            (IO_IN1, i.in1),
            (IO_IN2, i.in2),
            (IO_DSW1, i.dsw[0]),
            (IO_DSW2, i.dsw[1]),
            (IO_DSW3, i.dsw[2]),
            // eeprom DO high, buttons unpressed.
            (IO_PORTG, 0xFF),
        ] {
            self.dpram[off] = v;
        }
        // Daytona has no secondary control panel.
        for n in 0..4 {
            self.dpram[IO_ANALOG_SECONDARY + n] = 0xFF;
        }
        self.io_publish_lightguns();
    }

    /// Publishes the lightgun coordinates the way Virtua Cop's I/O board
    /// (model1io2) does: its FPGA hands the board five 16-bit words -- P1Y,
    /// P1X, P2Y, P2X (each a 10-bit ADC value) then an off-screen flag byte --
    /// and the board's Z80 drops them into the shared RAM at 0x80. Daytona
    /// never reads this block, so populating it unconditionally is harmless.
    fn io_publish_lightguns(&mut self) {
        let i = self.inputs;
        let le = |v: u16| [v as u8, (v >> 8) as u8];
        // P1 from the front end; P2 parked at its centre (single player).
        let words = [le(i.gun_y), le(i.gun_x), le(0x0e8), le(0x179)];
        for (n, w) in words.iter().enumerate() {
            self.dpram[0x80 + n * 2] = w[0];
            self.dpram[0x81 + n * 2] = w[1];
        }
        // Off-screen detect: bit 0 = P1, bit 1 = P2, zero on-screen.
        self.dpram[0x88] = u8::from(i.gun_offscreen);
        self.dpram[0x89] = 0x00;
    }

    /// Derives the monitor's transfer curve from the gamma curve the game has
    /// programmed into the colour translation RAM.
    ///
    /// There is no gamma table in the hardware. The colour translation RAM *is*
    /// the game's gamma curve, and it feeds the video DAC directly; what has to
    /// be modelled after it is the cabinet's monitor, which Sega calibrated at
    /// the factory for the specific game. That calibration is not in the ROM, so
    /// it has to be recovered from the only evidence there is -- the range of
    /// signals the game actually asks the DAC for.
    ///
    /// Daytona's curve emits 0, or 100..255, and nothing in 1..99. That gap is
    /// the CRT's cutoff: below roughly 100 the beam is off and the tube is black
    /// whatever the signal does, so the game keeps its whole range above it and
    /// uses 0 for blanking. A monitor set up for this game therefore has its
    /// black level at the pedestal and its gain such that 255 is peak white, so
    /// that is what this reproduces:
    ///
    ///   out = (signal - pedestal) * 255 / (255 - pedestal), clamped at 0
    ///
    /// No further power law is applied: a CRT's response is near enough to the
    /// sRGB curve a modern display decodes with that passing the signal straight
    /// through reproduces it.
    ///
    /// The reference uses this same shape with a fixed pedestal of 64 -- a compromise its
    /// author describes as producing "decent results for most games without
    /// requiring any game-specific adjustments". For Daytona that lifts the
    /// darkest lit signal to 48 of 255 instead of black, which is what makes the
    /// picture look washed out. Measuring the pedestal instead of assuming it
    /// costs nothing and is right for whichever game is running.
    pub fn rebuild_monitor(&mut self) {
        let mut pedestal = 255u32;
        for i in 0..(0xc000 / 2) {
            let w = self.colorxlat_ram[i >> 1];
            let v = ((w >> ((i & 1) * 16)) as u16 as u32) & 0xff;
            if v != 0 {
                pedestal = pedestal.min(v);
            }
        }
        // The measurement above is only evidence of a cutoff when the game
        // really does leave a gap above black. Sega Rally does not: its curve
        // emits every value from 1 up, so the minimum says nothing about the
        // tube and taking it literally leaves the signal untouched and the
        // picture far too bright. With no gap to measure, fall back to the
        // calibration the reference applies to every game -- black at 64, peak white at
        // 255, straight line between -- which reproduces its picture closely
        // (mean frame brightness within one level on Sega Rally's attract).
        const NO_GAP: u32 = 32;
        if pedestal < NO_GAP {
            self.monitor =
                std::array::from_fn(|i| (((i as f32 - 64.0) * 255.0 / 191.0).max(0.0)) as u8);
            return;
        }
        // A table that is empty or somehow spans nothing usable leaves the
        // signal alone rather than dividing by zero.
        if pedestal >= 255 {
            self.monitor = std::array::from_fn(|i| i as u8);
            return;
        }
        let span = (255 - pedestal) as f32;
        self.monitor = std::array::from_fn(|i| {
            let s = (i as u32).saturating_sub(pedestal) as f32 / span;
            (s.powf(CRT_GAMMA / SRGB_GAMMA) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        });
    }

    /// Exchanges one byte with the drive board, the way the I/O board's Z80
    /// does at 0x0170..0x01b9.
    ///
    /// The drive board is a Z80 of its own (SJ25-0207-01) driving the wheel's
    /// motor. We do not emulate it; what matters here is the command byte the
    /// game sends, which is the only description of what the wheel should be
    /// doing. `drive_cmd` carries it out to the front end, which is free to
    /// turn it into whatever the host can actually do -- a pad's rumble motors,
    /// say.
    ///
    /// When the game asks to read back, the reference answers 0xff (its
    /// `drive_read_cb` default), so an absent board reads as idle.
    fn io_service_drive(&mut self) {
        if self.dpram[IO_DRIVE_DIR] & 0x01 != 0 {
            self.dpram[IO_DRIVE_DATA] = 0xff;
        } else {
            let cmd = self.dpram[IO_DRIVE_DATA];
            if cmd != self.drive_cmd {
                log::info!(target: "drive", "{:02X} -> {:02X}", self.drive_cmd, cmd);
                self.drive_cmd = cmd;
            }
        }
    }

    /// Ends the frame: publishes any dirty colour table, parses the display
    /// list the game has just finished building, and raises the V-blank
    /// interrupt.
    pub fn trigger_vblank(&mut self) {
        if self.colorxlat_dirty {
            self.colorxlat_dirty = false;
            self.rebuild_monitor();
        }
        // video_ctl bit 0 selects the renderer's 30 Hz mode. The
        // geometrizer still runs asynchronously, but the rasterizer only
        // consumes/presents a new list on even video frames. Parsing the odd
        // frame reads the circular buffer while it is being rebuilt and was
        // the source of the intermittent exploded/inverted scenes.
        if self.video_ctl & 1 == 0 || self.frame_num & 1 == 0 {
            let mut geometry = std::mem::take(&mut self.geometry);
            // The parser walks a plain slice; snapshot the dual-port buffer
            // so the coprocessor worker can keep writing the next list while
            // this one is parsed.
            let buffer = self.buffer_ram.to_vec();
            geometry.parse(
                &buffer,
                &self.polygon_rom,
                &self.texture_rom,
                self.geo_read_start_address,
            );
            self.geometry = geometry;
        }
        self.frame_num = self.frame_num.wrapping_add(1);
        self.comm_tick();
        self.raise_irq(0);
    }
}
