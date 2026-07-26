//! Sega Model 2A sound board: a 68000 at 11.2896MHz plus the SCSP (YMF292-F).
//!
//! Same role as `sound.rs` (the Model 1 / model2-original board) but for the
//! 2A/2B/2C generation: the 68000 runs the game's sound driver, the SCSP is
//! the entire audio chip (32 PCM/FM slots plus a DSP), and the wire back to
//! the i960 is the SCSP's own MIDI port through the i8251.
//!
//! Layout:
//!
//! ```text
//!   000000-07ffff work RAM, shared with the SCSP ("soundram")
//!   100000-100fff SCSP registers (16-bit)
//!   400000-400001 model2snd_ctrl (sample banking; static with an 8MB region)
//!   600000-67ffff program ROM (audiocpu region)
//!   800000-9fffff samples + 0x000000
//!   a00000-dfffff samples + 0x200000 (bank4)
//!   e00000-ffffff samples + 0x600000 (bank5)
//! ```
//!
//! At reset the first 16 bytes of the program ROM are copied into soundram so
//! the 68000 finds its vectors there.

use crate::scsp::Scsp;
use m68000::cpu_details::Mc68000;
use m68000::exception::{Exception, Vector};
use m68000::memory_access::MemoryAccess;
use m68000::M68000;

/// 45.1584MHz crystal divided by four
/// a 45.1584 MHz crystal divided by four.
pub const SND2A_CPU_HZ: u32 = 45_158_400 / 4;

/// SCSP output rate: 22.5792MHz / 512.
pub const SCSP_SAMPLE_RATE: f32 = 44_100.0;

/// The board's memory map and devices, everything except the 68000 itself.
pub struct SoundBoard2A {
    /// "audiocpu": the sound driver, 512KB region (384KB populated on Sega
    /// Rally), mapped at 0x600000.
    pub rom: Vec<u8>,
    /// "samples": the 8MB sample ROM image, banked into the top of the map.
    pub samples: Vec<u8>,
    /// The sound chip. Owns the 512KB "soundram" shared with the 68000.
    pub scsp: Scsp,

    // --- UART, sound board -> main board (the SCSP's MIDI out) ---
    /// Byte latched out of the SCSP's MIDI-out FIFO, waiting for the i960.
    pub tx: Option<u8>,
    /// True while a byte handed to the board has not been clocked out yet.
    /// The i8251's TxRDY drops for the duration, and the main board's sound
    /// interrupt is asserted on its rising edge -- one interrupt per byte.
    pub tx_busy: bool,
    /// How many bytes the main board has sent us.
    pub rx_count: u64,
    /// model2snd_ctrl value (sample banking on regions larger than 8MB;
    /// Sega Rally's region is exactly 8MB, so the banks never move).
    pub snd_ctrl: u16,
}

impl SoundBoard2A {
    pub fn new(rom: Vec<u8>, samples: Vec<u8>) -> Self {
        Self {
            rom,
            samples,
            scsp: Scsp::new(),
            tx: None,
            tx_busy: false,
            rx_count: 0,
            snd_ctrl: 0,
        }
    }

    /// The i960 has put a byte on the wire: it arrives at the SCSP's MIDI in.
    pub fn uart_send(&mut self, data: u8) {
        self.rx_count += 1;
        log::trace!(target: "sound", "main -> driver: {:02X}", data);
        self.scsp.midi_in(data);
        self.tx_busy = true;
    }

    /// i8251 command with the reset bit set clears anything in flight.
    pub fn uart_control(&mut self, val: u8) {
        if val & 0x40 != 0 {
            self.tx = None;
        }
    }

    /// True while the driver has a byte waiting to be read back.
    pub fn uart_rx_ready(&self) -> bool {
        self.tx.is_some()
    }

    /// True while the UART can accept another byte for the board. Always here:
    /// `uart_send` hands the byte to the SCSP on the spot.
    pub fn uart_tx_ready(&self) -> bool {
        !self.tx_busy
    }

    /// Reads one byte of the board's address space (flat address, big endian).
    fn read8(&mut self, addr: u32) -> u8 {
        let a = addr & 0xffffff;
        match a {
            0x000000..=0x07ffff => self.scsp.ram[a as usize],
            0x100000..=0x100fff => {
                let v = self.scsp.read((a - 0x100000) >> 1);
                if a & 1 == 0 {
                    (v >> 8) as u8
                } else {
                    v as u8
                }
            }
            0x600000..=0x67ffff => self
                .rom
                .get((a - 0x600000) as usize)
                .copied()
                .unwrap_or(0xff),
            0x800000..=0x9fffff => self
                .samples
                .get((a - 0x800000) as usize)
                .copied()
                .unwrap_or(0xff),
            0xa00000..=0xdfffff => self
                .samples
                .get((a - 0xa00000 + 0x200000) as usize)
                .copied()
                .unwrap_or(0xff),
            0xe00000..=0xffffff => self
                .samples
                .get((a - 0xe00000 + 0x600000) as usize)
                .copied()
                .unwrap_or(0xff),
            _ => 0,
        }
    }

    fn write8(&mut self, addr: u32, val: u8) {
        let a = addr & 0xffffff;
        match a {
            0x000000..=0x07ffff => self.scsp.ram[a as usize] = val,
            0x100000..=0x100fff => {
                let (mask, data) = if a & 1 == 0 {
                    (0xff00u16, (val as u16) << 8)
                } else {
                    (0x00ffu16, val as u16)
                };
                self.scsp.write((a - 0x100000) >> 1, data, mask);
            }
            0x400000..=0x400001 if a & 1 == 1 => {
                self.snd_ctrl = val as u16;
            }
            _ => {}
        }
    }
}

impl MemoryAccess for SoundBoard2A {
    fn get_byte(&mut self, addr: u32) -> Option<u8> {
        Some(self.read8(addr))
    }

    fn get_word(&mut self, addr: u32) -> Option<u16> {
        // An aligned word inside the SCSP register window must be a single
        // 16-bit access: some register reads have side effects (the MIDI-in
        // register pops the FIFO), so byte-pair assembly would fire them twice.
        let a = addr & 0xffffff;
        if a & 1 == 0 && (0x100000..0x101000).contains(&a) {
            return Some(self.scsp.read((a - 0x100000) >> 1));
        }
        Some(((self.read8(addr) as u16) << 8) | self.read8(addr.wrapping_add(1)) as u16)
    }

    fn set_byte(&mut self, addr: u32, value: u8) -> Option<()> {
        self.write8(addr, value);
        Some(())
    }

    fn set_word(&mut self, addr: u32, value: u16) -> Option<()> {
        let a = addr & 0xffffff;
        if a & 1 == 0 && (0x100000..0x101000).contains(&a) {
            self.scsp.write((a - 0x100000) >> 1, value, 0xffff);
            return Some(());
        }
        self.write8(addr, (value >> 8) as u8);
        self.write8(addr.wrapping_add(1), value as u8);
        Some(())
    }

    fn reset_instruction(&mut self) {}
}

/// The board with its CPU attached.
pub struct SoundSystem2A {
    pub cpu: M68000<Mc68000>,
    pub board: SoundBoard2A,
    /// Cycle budget carried between slices, since the 68000 runs at its own
    /// clock rather than the i960's.
    remainder: i64,
    /// Fractional sample-clock accumulator (SCSP samples per 68000 cycle).
    sample_acc: f64,
    /// Rendered stereo output at the SCSP rate, drained by the front end.
    /// Headless tools never drain it, so it is capped rather than unbounded.
    pub samples: std::collections::VecDeque<(i16, i16)>,
    /// Exceptions raised by executed opcodes, indexed by vector. SCSP
    /// interrupts are injected separately and do not appear here.
    pub exception_counts: [u64; 256],
}

/// Upper bound on buffered audio (~2s) so headless runs don't accumulate it.
const MAX_BUFFERED_SAMPLES: usize = 90_000;

impl SoundSystem2A {
    pub fn new(rom: Vec<u8>, samples: Vec<u8>) -> Self {
        let mut board = SoundBoard2A::new(rom, samples);
        // reset_model2_scsp: copy the 68k vector table into RAM before the
        // CPU reset reads it through the bus.
        board.scsp.ram[..16].copy_from_slice(&board.rom[..16]);
        let cpu = {
            let mut c: M68000<Mc68000> = M68000::new();
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
            sample_acc: 0.0,
            samples: std::collections::VecDeque::new(),
            exception_counts: [0; 256],
        }
    }

    /// Output sample rate: the SCSP's native 44.1kHz.
    pub fn sample_rate(&self) -> f32 {
        SCSP_SAMPLE_RATE
    }

    /// Hands the driver a byte from the i960.
    pub fn send(&mut self, data: u8) {
        self.board.uart_send(data);
    }

    /// Advances the SCSP by the amount of 68000 time that just elapsed.
    ///
    /// Runs alongside the CPU rather than once per main-board slice for the
    /// same reason as the MultiPCM board: register writes are stream barriers,
    /// and key-on/key-off events must be rendered against the state they were
    /// issued in.
    fn render_cycles(&mut self, cycles: usize) {
        self.sample_acc += cycles as f64 * SCSP_SAMPLE_RATE as f64 / SND2A_CPU_HZ as f64;
        while self.sample_acc >= 1.0 {
            self.sample_acc -= 1.0;
            let (l, r) = self.board.scsp.generate();
            let l = l.clamp(-32768, 32767) as i16;
            let r = r.clamp(-32768, 32767) as i16;
            if self.samples.len() < MAX_BUFFERED_SAMPLES {
                self.samples.push_back((l, r));
            }
        }
        self.board.tx_busy = false;
        // Move anything the SCSP transmitted into the UART's receive latch.
        if self.board.tx.is_none() {
            self.board.tx = self.board.scsp.midi_out_pop();
            if let Some(b) = self.board.tx {
                log::trace!(target: "sound", "driver -> main: {b:02X}");
            }
        }
    }

    /// Runs the board for `i960_cycles` of main-board time.
    pub fn run(&mut self, i960_cycles: i32, i960_hz: u32) {
        self.remainder += i960_cycles as i64 * SND2A_CPU_HZ as i64 / i960_hz as i64;
        while self.remainder > 0 {
            let (used, exception) = self.cpu.interpreter_exception(&mut self.board);
            if let Some(vector) = exception {
                self.exception_counts[vector as usize] += 1;
                self.cpu.exception(Exception::from(vector));
            }
            // The SCSP's interrupt lines are level-triggered: while a line is
            // asserted above the 68000's mask, take it. Entering the handler
            // raises the mask, so this cannot re-enter the same level until
            // the RTE drops it again.
            let lines = self.board.scsp.irq_lines();
            for line in (1..=7usize).rev() {
                if lines[line] && line as u8 > self.cpu.regs.sr.interrupt_mask {
                    self.cpu.exception(Exception::from(
                        Vector::Level1Interrupt as u8 + line as u8 - 1,
                    ));
                    break;
                }
            }
            // A stopped CPU reports no cycles; do not spin on it.
            if used == 0 {
                let rest = self.remainder as usize;
                self.remainder = 0;
                self.render_cycles(rest);
            } else {
                self.remainder -= used as i64;
                self.render_cycles(used);
            }
        }
    }
}
