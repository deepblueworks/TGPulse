//! Sega Model 1 sound board (`segam1audio`), as fitted to Daytona.
//!
//! A 68000 at 10MHz with its own program ROM, two MultiPCM samplers, a YM3438
//! for FM, and an i8251 UART that is the only wire back to the i960. The main
//! board sends it one byte at a time and it does the rest on its own.
//!
//! This is emulated rather than reimplemented because the program is the game's
//! own: `epr-16720`/`epr-16721` are Daytona's sound driver, so an HLE would
//! mean rewriting that driver by hand for every Model 2 title. Same reasoning as
//! the TGP.
//!
//! Layout:
//!
//! ```text
//!   000000-03ffff program ROM
//!   080000-09ffff mirror of the upper ROM socket (sndcpu + 0x20000)
//!   c20000-c20003 i8251 UART        (odd bytes only)
//!   c40000-c40007 MultiPCM 1        (odd bytes only)
//!   c50000-c50001 MultiPCM 1 bank
//!   c60000-c60007 MultiPCM 2        (odd bytes only)
//!   c70000-c70001 MultiPCM 2 bank
//!   d00000-d00007 YM3438            (odd bytes only)
//!   f00000-f0ffff work RAM
//! ```

use crate::multipcm::MultiPcm;
use m68000::cpu_details::Mc68000;
use m68000::exception::{Exception, Vector};
use m68000::memory_access::MemoryAccess;
use m68000::M68000;

/// 20MHz crystal divided by two.
pub const SND_CPU_HZ: u32 = 10_000_000;

/// The board's RAM: the reference maps 0xf00000-0xf0ffff, noting the real PCB carries
/// two 8Kx8 SRAMs.
const SND_RAM_SIZE: usize = 0x10000;

/// i8251 status bits the driver polls. TxRDY and TxEMPTY are always true here:
/// we consume a byte the instant it is written, so the transmitter is never
/// busy. RxRDY is raised only when the main board has actually sent something.
const UART_TX_RDY: u8 = 0x01;
const UART_RX_RDY: u8 = 0x02;
const UART_TX_EMPTY: u8 = 0x04;

/// Timing/status portion of the YM3438 (OPN2). The sound driver uses these
/// timers as a timebase for its sequencer, independently of the chip's FM
/// output. Leaving status hard-wired to zero lets the UART-driven engine
/// updater run but strands music and speech waiting for a timer overflow.
#[derive(Default)]
struct Ym3438Timers {
    address: [u8; 2],
    timer_a: u16,
    timer_b: u8,
    mode: u8,
    status: u8,
    clocks_a: u64,
    clocks_b: u64,
    /// 8MHz YM clock expressed as fifths of a 10MHz 68000 cycle.
    clock_fifths: u64,
}

impl Ym3438Timers {
    fn write_address(&mut self, bank: usize, value: u8) {
        self.address[bank] = value;
    }

    fn write_data(&mut self, bank: usize, value: u8) {
        // Timer/mode registers live on port 0 only.
        if bank != 0 {
            return;
        }
        match self.address[0] {
            0x24 => self.timer_a = (self.timer_a & 0x0003) | ((value as u16) << 2),
            0x25 => self.timer_a = (self.timer_a & 0x03fc) | (value as u16 & 3),
            0x26 => self.timer_b = value,
            0x27 => {
                if value & 0x10 != 0 {
                    self.status &= !0x01;
                }
                if value & 0x20 != 0 {
                    self.status &= !0x02;
                }
                // Loading a timer starts a fresh period on the hardware.
                if value & 0x01 != 0 && self.mode & 0x01 == 0 {
                    self.clocks_a = 0;
                }
                if value & 0x02 != 0 && self.mode & 0x02 == 0 {
                    self.clocks_b = 0;
                }
                self.mode = value;
            }
            _ => {}
        }
    }

    fn status(&self) -> u8 {
        self.status
    }

    fn advance_68000_cycles(&mut self, cycles: usize) {
        // YM3438 clock is 8MHz while the board CPU is 10MHz.
        self.clock_fifths += cycles as u64 * 4;
        let clocks = self.clock_fifths / 5;
        self.clock_fifths %= 5;

        if self.mode & 0x01 != 0 {
            self.clocks_a += clocks;
            // YMFM: period * OPERATORS * clock_prescale. YM3438 has six
            // channels/four operators (24) and fixed prescale 6 => 144.
            let period = (1024u64 - self.timer_a as u64) * 144;
            while self.clocks_a >= period.max(1) {
                self.clocks_a -= period.max(1);
                if self.mode & 0x04 != 0 {
                    self.status |= 0x01;
                }
            }
        }
        if self.mode & 0x02 != 0 {
            self.clocks_b += clocks;
            // Timer B has an additional factor of 16: 16 * 24 * 6 = 2304.
            let period = (256u64 - self.timer_b as u64) * 2304;
            while self.clocks_b >= period.max(1) {
                self.clocks_b -= period.max(1);
                if self.mode & 0x08 != 0 {
                    self.status |= 0x02;
                }
            }
        }
    }
}

/// The board's memory map and devices, everything except the 68000 itself.
pub struct SoundBoard {
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
    /// The two samplers. Between them they carry Daytona's entire mix bar the
    /// YM3438's FM (see `ym_writes`).
    pub pcm: [MultiPcm; 2],

    // --- i8251, main board -> sound board ---
    /// Bytes received from the i960 and not yet read by the driver. A queue,
    /// not a single slot: back-to-back command bytes (VF's sound handshake
    /// sends three in a row) must not overwrite each other.
    pub rx: std::collections::VecDeque<u8>,
    /// How many bytes the main board has sent us.
    pub rx_count: u64,
    /// How many of those the driver actually collected from the data register.
    pub rx_read_count: u64,
    /// Byte the driver sent back, waiting for the i960 to collect it.
    pub tx: Option<u8>,

    /// YM3438 traffic. The FM chip is not implemented; counting its writes
    /// keeps "why is this part silent" answerable instead of mysterious.
    pub ym_writes: u64,
    ym: Ym3438Timers,
}

impl SoundBoard {
    pub fn new(rom: Vec<u8>, pcm1: Vec<u8>, pcm2: Vec<u8>) -> Self {
        Self {
            rom,
            ram: vec![0; SND_RAM_SIZE],
            pcm: [
                MultiPcm::new(pcm1, SND_CPU_HZ as f32),
                MultiPcm::new(pcm2, SND_CPU_HZ as f32),
            ],
            rx: std::collections::VecDeque::new(),
            rx_count: 0,
            rx_read_count: 0,
            tx: None,
            ym_writes: 0,
            ym: Ym3438Timers::default(),
        }
    }

    /// The i960 has put a byte on the wire.
    pub fn uart_send(&mut self, data: u8) {
        self.rx.push_back(data);
        if self.rx.len() > 8 {
            self.rx.pop_front();
        }
        self.rx_count += 1;
        log::trace!(target: "sound", "main -> driver: {:02X}", data);
    }

    /// i8251 mode/command register. The driver's framing does not matter to us
    /// -- we move whole bytes -- but a command with the reset bit set clears
    /// anything in flight, which does.
    pub fn uart_control(&mut self, val: u8) {
        log::trace!(target: "sound", "main uart ctl: {:02X}", val);
        // Command register bit 6 = internal reset.
        if val & 0x40 != 0 {
            self.rx.clear();
            self.tx = None;
        }
    }

    /// True while the driver has a byte waiting to be read back.
    pub fn uart_rx_ready(&self) -> bool {
        self.tx.is_some()
    }

    /// True while the main board has a byte waiting for the driver.
    pub fn uart_rx_full(&self) -> bool {
        !self.rx.is_empty()
    }

    /// True while the UART can accept another byte for the board.
    ///
    /// Always, here: `uart_send` hands the byte over on the spot, so the
    /// transmitter is never busy. This matters more than it looks -- the main
    /// board's sound interrupt is asserted on TxRDY *or* RxRDY, so this line is
    /// what keeps the game's sound task running at all.
    pub fn uart_tx_ready(&self) -> bool {
        true
    }

    fn uart_status(&self) -> u8 {
        let mut s = UART_TX_RDY | UART_TX_EMPTY;
        if !self.rx.is_empty() {
            s |= UART_RX_RDY;
        }
        s
    }

    /// Reads one byte of the board's address space. The 68000's byte lanes are
    /// handled by the caller; this takes a flat address.
    fn read8(&mut self, addr: u32) -> u8 {
        let a = addr & 0xffffff;
        match a {
            0x000000..=0x03ffff => self.rom.get(a as usize).copied().unwrap_or(0xff),
            // Mirror of the upper ROM socket.
            0x080000..=0x09ffff => self
                .rom
                .get((a - 0x080000 + 0x20000) as usize)
                .copied()
                .unwrap_or(0xff),
            0xf00000..=0xf0ffff => self.ram[(a - 0xf00000) as usize],

            // i8251: even register = data, odd = status (odd bytes of the word).
            0xc20001 => {
                if !self.rx.is_empty() {
                    self.rx_read_count += 1;
                }
                let v = self.rx.pop_front().unwrap_or(0);
                log::trace!(target: "sound", "driver reads cmd: {:02X}", v);
                v
            }
            0xc20003 => self.uart_status(),

            0xc40001..=0xc40007 if a & 1 == 1 => self.pcm[0].read(),
            0xc60001..=0xc60007 if a & 1 == 1 => self.pcm[1].read(),
            // YM3438: address/status A, data A, address/status B, data B,
            // all on the low byte lane of successive 16-bit words.
            0xd00001 | 0xd00005 => self.ym.status(),
            0xd00003 | 0xd00007 => 0,
            _ => 0,
        }
    }

    fn write8(&mut self, addr: u32, val: u8) {
        let a = addr & 0xffffff;
        match a {
            0xf00000..=0xf0ffff => self.ram[(a - 0xf00000) as usize] = val,

            0xc20001 => {
                log::trace!(target: "sound", "driver -> main: {:02X}", val);
                self.tx = Some(val);
            }
            // Mode/command register: the driver configures the UART here. We
            // have no framing to configure, so there is nothing to keep.
            0xc20003 => {}

            0xc50000..=0xc50001 => self.pcm[0].set_bank(val as u32),
            0xc70000..=0xc70001 => self.pcm[1].set_bank(val as u32),
            0xc40012..=0xc40013 => {} // the reference: nopw

            // The chips sit on the odd byte lanes (the reference: umask16(0x00ff)), so
            // 68000 address 0xc40001 is chip offset 0, 0xc40003 offset 1,...
            0xc40001..=0xc40007 if a & 1 == 1 => self.pcm[0].write((a - 0xc40001) >> 1, val),
            0xc60001..=0xc60007 if a & 1 == 1 => self.pcm[1].write((a - 0xc60001) >> 1, val),
            0xd00001 => {
                self.ym_writes += 1;
                self.ym.write_address(0, val);
            }
            0xd00003 => {
                self.ym_writes += 1;
                self.ym.write_data(0, val);
            }
            0xd00005 => {
                self.ym_writes += 1;
                self.ym.write_address(1, val);
            }
            0xd00007 => {
                self.ym_writes += 1;
                self.ym.write_data(1, val);
            }
            0x000000..=0x09ffff => {} // ROM
            _ => {}
        }
    }
}

impl MemoryAccess for SoundBoard {
    fn get_byte(&mut self, addr: u32) -> Option<u8> {
        Some(self.read8(addr))
    }
    fn get_word(&mut self, addr: u32) -> Option<u16> {
        // The 68000 is big endian.
        Some(((self.read8(addr) as u16) << 8) | self.read8(addr.wrapping_add(1)) as u16)
    }
    fn set_byte(&mut self, addr: u32, value: u8) -> Option<()> {
        self.write8(addr, value);
        Some(())
    }
    fn set_word(&mut self, addr: u32, value: u16) -> Option<()> {
        self.write8(addr, (value >> 8) as u8);
        self.write8(addr.wrapping_add(1), value as u8);
        Some(())
    }
    fn reset_instruction(&mut self) {}
}

/// The board with its CPU attached.
pub struct SoundSystem {
    pub cpu: M68000<Mc68000>,
    pub board: SoundBoard,
    /// Cycle budget carried between slices, since the 68000 runs at its own
    /// clock rather than the i960's.
    remainder: i64,
    /// The UART's rxrdy line is wired to the 68000's IRQ 2.
    irq_pending: bool,
    /// Fractional sample-clock accumulator (chip samples per 68000 cycle).
    sample_acc: f64,
    /// Rendered stereo output at the chip rate, drained by the front end.
    /// Headless tools never drain it, so it is capped rather than unbounded.
    pub samples: std::collections::VecDeque<(i16, i16)>,
    /// Exceptions raised by executed opcodes, indexed by vector. IRQ2 is
    /// injected separately and therefore does not appear here.
    pub exception_counts: [u64; 256],
}

/// Upper bound on buffered audio (~2s) so headless runs don't accumulate it.
const MAX_BUFFERED_SAMPLES: usize = 90_000;

impl SoundSystem {
    pub fn new(rom: Vec<u8>, pcm1: Vec<u8>, pcm2: Vec<u8>) -> Self {
        let mut board = SoundBoard::new(rom, pcm1, pcm2);
        // M68000::new() resets, which reads the vectors through the bus.
        let cpu = {
            let mut c: M68000<Mc68000> = M68000::new();
            // Force the reset to happen against our map straight away, so the
            // vectors are reported now rather than on the first instruction.
            c.interpreter(&mut board);
            c
        };
        log::debug!(
            target: "sound",
            "68000 reset: ssp={:08X} pc={:08X}",
            cpu.regs.ssp.0,
            cpu.regs.pc.0
        );
        Self {
            cpu,
            board,
            remainder: 0,
            irq_pending: false,
            sample_acc: 0.0,
            samples: std::collections::VecDeque::new(),
            exception_counts: [0; 256],
        }
    }

    /// Output sample rate: the MultiPCMs' native clock (10MHz / 224).
    pub fn sample_rate(&self) -> f32 {
        self.board.pcm[0].sample_rate()
    }

    /// Hands the driver a byte from the i960 and raises the UART's interrupt,.
    pub fn send(&mut self, data: u8) {
        self.board.uart_send(data);
        self.irq_pending = true;
    }

    /// Advances the two DACs by the amount of 68000 time that just elapsed.
    ///
    /// This intentionally runs alongside the CPU, rather than once after an
    /// entire main-board slice. MultiPCM register writes are stream barriers
    /// in the reference: audio before a key-on/key-off/bank change must be rendered with
    /// the old state. Rendering a whole video frame from the final register
    /// state erased short speech/music events and made the continuously-updated
    /// engine voice dominate the complete frame.
    fn render_cycles(&mut self, cycles: usize) {
        self.sample_acc +=
            cycles as f64 * self.board.pcm[0].sample_rate() as f64 / SND_CPU_HZ as f64;
        while self.sample_acc >= 1.0 {
            self.sample_acc -= 1.0;
            let (l1, r1) = self.board.pcm[0].generate();
            let (l2, r2) = self.board.pcm[1].generate();

            // Each MultiPCM stream clamps to signed 16-bit first; the board
            // mixer then routes each at 0.5. Summing the raw accumulators
            // before clamping lets a continuously driven engine voice swamp
            // everything else and is not how the reference stream routing works.
            let l1 = l1.clamp(-32768, 32767);
            let r1 = r1.clamp(-32768, 32767);
            let l2 = l2.clamp(-32768, 32767);
            let r2 = r2.clamp(-32768, 32767);
            let l = ((l1 + l2) / 2) as i16;
            let r = ((r1 + r2) / 2) as i16;
            if self.samples.len() < MAX_BUFFERED_SAMPLES {
                self.samples.push_back((l, r));
            }
        }
    }

    /// Runs the board for `i960_cycles` of main-board time.
    pub fn run(&mut self, i960_cycles: i32, i960_hz: u32) {
        // Convert the main board's budget into this board's clock.
        self.remainder += i960_cycles as i64 * SND_CPU_HZ as i64 / i960_hz as i64;
        // The 8251's rxrdy line drives the driver's level-2 interrupt. Assert
        // it when a byte arrives; then re-assert it (edge per byte) each time
        // the driver actually consumes one while more remain, so a burst -- VF
        // sends three command bytes back to back -- drains one interrupt at a
        // time. Asserting unconditionally while the FIFO is non-empty instead
        // leaves a stale level-2 pending that fires again after the byte is
        // read, and the driver then services a phantom 0 and stops making sound.
        if self.irq_pending {
            self.irq_pending = false;
            self.cpu
                .exception(Exception::from(Vector::Level2Interrupt as u8));
        }
        while self.remainder > 0 {
            let reads_before = self.board.rx_read_count;
            let (used, exception) = self.cpu.interpreter_exception(&mut self.board);
            if self.board.rx_read_count > reads_before && !self.board.rx.is_empty() {
                self.cpu
                    .exception(Exception::from(Vector::Level2Interrupt as u8));
            }
            if let Some(vector) = exception {
                self.exception_counts[vector as usize] += 1;
                self.cpu.exception(Exception::from(vector));
            }
            // A stopped CPU reports no cycles; do not spin on it.
            if used == 0 {
                let rest = self.remainder as usize;
                self.remainder = 0;
                self.board.ym.advance_68000_cycles(rest);
                self.render_cycles(rest);
            } else {
                self.remainder -= used as i64;
                self.board.ym.advance_68000_cycles(used);
                self.render_cycles(used);
            }
        }
    }
}

/// Either of the two sound boards a Model 2 game can ship with: the
/// segam1audio MultiPCM board (original Model 2, e.g. Daytona) or the
/// SCSP board (Model 2A/2B/2C, e.g. Sega Rally). The main board talks to both
/// through the same i8251 UART, so this exposes the union of what the memory
/// map and front end use.
pub enum Sound {
    // Boxed because the two boards differ by several hundred kilobytes of
    // sample RAM and voice state, and every `Sound` would otherwise be as big
    // as the larger of them.
    MultiPcm(Box<SoundSystem>),
    Scsp(Box<crate::sound2a::SoundSystem2A>),
}

impl Sound {
    /// The i960 has put a byte on the wire.
    pub fn send(&mut self, data: u8) {
        match self {
            Sound::MultiPcm(s) => s.send(data),
            Sound::Scsp(s) => s.send(data),
        }
    }

    /// i8251 mode/command register write from the i960 side.
    pub fn control(&mut self, val: u8) {
        match self {
            Sound::MultiPcm(s) => s.board.uart_control(val),
            Sound::Scsp(s) => s.board.uart_control(val),
        }
    }

    /// Takes the byte the driver sent back, if any (a read of the UART's data
    /// register consumes it).
    pub fn take_reply(&mut self) -> u8 {
        match self {
            Sound::MultiPcm(s) => s.board.tx.take().unwrap_or(0),
            Sound::Scsp(s) => s.board.tx.take().unwrap_or(0),
        }
    }

    /// True while the driver has a byte waiting to be read back.
    pub fn reply_ready(&self) -> bool {
        match self {
            Sound::MultiPcm(s) => s.board.uart_rx_ready(),
            Sound::Scsp(s) => s.board.uart_rx_ready(),
        }
    }

    /// True while the UART can accept another byte for the board.
    pub fn tx_ready(&self) -> bool {
        match self {
            Sound::MultiPcm(s) => s.board.uart_tx_ready(),
            Sound::Scsp(s) => s.board.uart_tx_ready(),
        }
    }

    /// Runs the board for `i960_cycles` of main-board time.
    pub fn run(&mut self, i960_cycles: i32, i960_hz: u32) {
        match self {
            Sound::MultiPcm(s) => s.run(i960_cycles, i960_hz),
            Sound::Scsp(s) => s.run(i960_cycles, i960_hz),
        }
    }

    pub fn sample_rate(&self) -> f32 {
        match self {
            Sound::MultiPcm(s) => s.sample_rate(),
            Sound::Scsp(s) => s.sample_rate(),
        }
    }

    /// Drains the rendered stereo output.
    pub fn drain_samples(&mut self) -> std::collections::vec_deque::Drain<'_, (i16, i16)> {
        match self {
            Sound::MultiPcm(s) => s.samples.drain(..),
            Sound::Scsp(s) => s.samples.drain(..),
        }
    }

    /// The segam1audio board, for tools that report MultiPCM-specific stats.
    pub fn as_multi_pcm(&self) -> Option<&SoundSystem> {
        match self {
            Sound::MultiPcm(s) => Some(s),
            Sound::Scsp(_) => None,
        }
    }

    /// The SCSP board, for tools that report 2A-specific stats.
    pub fn as_scsp(&self) -> Option<&crate::sound2a::SoundSystem2A> {
        match self {
            Sound::MultiPcm(_) => None,
            Sound::Scsp(s) => Some(s),
        }
    }
}
