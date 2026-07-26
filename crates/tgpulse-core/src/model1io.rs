//! The Model 1 I/O board, 837-8950-01.
//!
//! A Z80 with its own firmware, a Sega 315-5338A I/O chip, a 93C45 serial
//! EEPROM and an MSM6253 ADC, sitting between the main board and the cabinet.
//! The V60 never touches a control directly: it leaves a command in the shared
//! dual-port RAM and the Z80 fills the rest of that RAM in with the state of
//! the panel.
//!
//! It matters for more than inputs. The operator settings a game keeps -- the
//! country, which is what selects the language -- live in the 93C45 on this
//! board, and the only thing that can write them is this Z80. A faked board
//! can poll inputs convincingly and still leave a game unable to remember that
//! it was set to English, because the commit never happens.
//!
//!

use std::cell::Cell;

use z80::{Z80_io, Z80};

use crate::config::Inputs;
use crate::eeprom93c46::Eeprom93c46;

/// Z80 clock: the board's 32 MHz crystal divided by eight.
pub const Z80_HZ: u32 = 4_000_000;

/// The dual-port RAM shared with the main board (an MB8421).
pub const DPRAM_SIZE: usize = 0x800;

/// Everything the Z80 can reach.
struct Board {
    rom: Vec<u8>,
    /// MB8464, 8KB at 0x4000.
    ram: [u8; 0x2000],
    io: Chip5338,
    adc: Adc,

    /// Shared with the V60. The board lives here rather than in the main
    /// system because the dual-port RAM is physically between the two.
    dpram: Vec<u8>,

    /// Cabinet state, refreshed by the main system each frame.
    inputs: Inputs,
    eeprom: Eeprom93c46,

    /// Last value written to port E, which drives the force-feedback board.
    drive_cmd: u8,
    /// Last value written to port F: lamps and the coin counter.
    outputs: u8,
    /// Port A bit 0 swaps the panel for the second set of controls on a twin
    /// cabinet, and swaps the DIP switches in for the digital inputs.
    secondary_controls: bool,
}

pub struct IoBoard {
    cpu: Z80<Board>,
    /// Z80 cycles owed but not yet run, so a fractional slice is not lost.
    cycle_debt: i64,
}

impl IoBoard {
    /// Builds the board. Without firmware there is no board: the caller is
    /// expected to have loaded the romset's `iocpu` region.
    pub fn new(firmware: &[u8], eeprom: Eeprom93c46) -> Self {
        let mut rom = firmware.to_vec();
        // Only the bottom 16KB is mapped; the EPROM is larger than the window.
        rom.resize(0x4000, 0xff);

        let mut cpu = Z80::new(Board {
            rom,
            ram: [0; 0x2000],
            io: Chip5338::default(),
            adc: Adc::default(),
            dpram: vec![0; DPRAM_SIZE],
            inputs: Inputs::default(),
            eeprom,
            drive_cmd: 0xff,
            outputs: 0,
            secondary_controls: false,
        });
        cpu.reset();

        Self { cpu, cycle_debt: 0 }
    }

    /// Runs the board for `cycles` of its own clock.
    pub fn run(&mut self, cycles: i64) {
        self.cycle_debt += cycles;
        while self.cycle_debt > 0 {
            self.cycle_debt -= self.cpu.step() as i64;
        }
    }

    /// Publishes the panel state the firmware will read on its next poll.
    pub fn set_inputs(&mut self, inputs: Inputs) {
        self.cpu.io.inputs = inputs;
    }

    /// The force command the game last sent to the drive board.
    pub fn drive_cmd(&self) -> u8 {
        self.cpu.io.drive_cmd
    }

    pub fn dpram(&self) -> &[u8] {
        &self.cpu.io.dpram
    }

    pub fn dpram_mut(&mut self) -> &mut Vec<u8> {
        &mut self.cpu.io.dpram
    }

    pub fn eeprom(&self) -> &Eeprom93c46 {
        &self.cpu.io.eeprom
    }

    pub fn eeprom_mut(&mut self) -> &mut Eeprom93c46 {
        &mut self.cpu.io.eeprom
    }
}

impl Z80_io for Board {
    fn read_byte(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3fff => self.rom[addr as usize],
            0x4000..=0x5fff => self.ram[(addr - 0x4000) as usize],
            0x8000..=0x800f => self.io.read(self, (addr & 0x0f) as u8),
            0xc000..=0xc003 => self.adc.shift_out(),
            _ => 0xff,
        }
    }

    fn write_byte(&mut self, addr: u16, value: u8) {
        match addr {
            0x4000..=0x5fff => self.ram[(addr - 0x4000) as usize] = value,
            0x8000..=0x800f => {
                let reg = (addr & 0x0f) as u8;
                self.io_write(reg, value);
            }
            // Writing picks a channel and latches its reading in one action.
            0xc000..=0xc003 => {
                let value = self.analog((addr & 3) as usize);
                self.adc.latch(value);
            }
            _ => {}
        }
    }
}

impl Board {
    /// The digital input ports, in the order the 315-5338A reads them. On a
    /// twin cabinet port A bit 0 swaps in the DIP switches instead.
    fn input_port(&self, port: u8) -> u8 {
        let i = &self.inputs;
        match (port, self.secondary_controls) {
            (1, false) => i.in0,
            (2, false) => i.in1,
            (3, false) => i.in2,
            (1, true) => i.dsw[0],
            (2, true) => i.dsw[1],
            (3, true) => i.dsw[2],
            // Port E reads the drive board back; nothing here models one, and
            // the firmware only checks that it answers.
            (4, _) => 0xff,
            // Port G: the EEPROM's data line, then the four board buttons,
            // which are not wired to anything a player can press.
            (6, _) => (u8::from(self.eeprom.do_read()) << 7) | 0x7f,
            _ => 0xff,
        }
    }

    /// One ADC channel's reading. Channels 4 to 7 are the second set of
    /// controls on a twin cabinet, which port A selects between.
    fn analog(&self, channel: usize) -> u8 {
        let i = &self.inputs;
        match channel + usize::from(self.secondary_controls) * 4 {
            0 => i.steer,
            1 => i.accel,
            2 => i.brake,
            other => i.analog.get(other).copied().unwrap_or(0),
        }
    }

    fn io_write(&mut self, reg: u8, value: u8) {
        match reg {
            0x00..=0x06 => {
                self.io.port_value[reg as usize] = value;
                self.output_port(reg, value);
            }
            0x08 => {
                // Direction register: a bit set means the port is an input.
                // A port that has just become an output re-presents its latch.
                let changed = value ^ self.io.port_config;
                self.io.port_config = value;
                for port in 0..7u8 {
                    if changed & (1 << port) != 0 && value & (1 << port) == 0 {
                        self.output_port(port, self.io.port_value[port as usize]);
                    }
                }
            }
            0x09 => self.command(value),
            0x0a => self.io.serial_output = value,
            _ => {}
        }
    }

    /// The command register. The chip addresses the dual-port RAM a byte at a
    /// time: the address is loaded in two halves from the serial latch, then a
    /// command moves one byte in or out.
    fn command(&mut self, value: u8) {
        self.io.cmd = value;
        match value {
            0x00 => self.io.address = (self.io.address & 0xff00) | self.io.serial_output as u16,
            0x01 => {
                self.io.address = (self.io.address & 0x00ff) | ((self.io.serial_output as u16) << 8)
            }
            0x07 => {
                let (address, data) = (self.io.address, self.io.serial_output);
                self.dpram_write(address, data);
            }
            0x70..=0x77 => {
                let data = self.io.serial_output;
                self.dpram_write((value & 0x07) as u16, data);
            }
            // Sent once the address is set and the chip is about to be read.
            0x87 => {}
            other => log::debug!(target: "ioboard", "unknown 315-5338A command {other:02X}"),
        }
    }

    fn dpram_write(&mut self, address: u16, data: u8) {
        if let Some(slot) = self.dpram.get_mut(address as usize & (DPRAM_SIZE - 1)) {
            *slot = data;
        }
    }

    fn output_port(&mut self, port: u8, value: u8) {
        match port {
            0 => {
                // 7 eeprom clk, 6 eeprom cs, 5 eeprom di, 4 eeprom pe,
                // 1 led, 0 which set of controls the panel presents.
                self.eeprom.cs_write(value & 0x40 != 0);
                self.eeprom.di_write(value & 0x20 != 0);
                self.eeprom.clk_write(value & 0x80 != 0);
                self.secondary_controls = value & 0x01 != 0;
            }
            4 => self.drive_cmd = value,
            5 => self.outputs = value,
            _ => {}
        }
    }
}

/// Sega 315-5338A: seven parallel ports plus a serial path into the host's
/// memory, which on this board is the dual-port RAM.
#[derive(Default)]
struct Chip5338 {
    port_value: [u8; 7],
    /// A set bit marks that port as an input.
    port_config: u8,
    cmd: u8,
    serial_output: u8,
    address: u16,
}

impl Chip5338 {
    fn read(&self, board: &Board, reg: u8) -> u8 {
        match reg {
            0x00..=0x06 => {
                if self.port_config & (1 << reg) != 0 {
                    board.input_port(reg)
                } else {
                    self.port_value[reg as usize]
                }
            }
            0x08 => self.port_config,
            0x0a => self.serial_output,
            0x0b => self.cmd,
            0x0c => board
                .dpram
                .get(self.address as usize & (DPRAM_SIZE - 1))
                .copied()
                .unwrap_or(0xff),
            // Status: bit 3 says the transfer finished, bit 0 that the last
            // command was acknowledged. Both are always true here, because
            // nothing in this model takes time.
            0x0d => 0x08,
            _ => 0xff,
        }
    }
}

/// OKI MSM6253: four multiplexed analog inputs, read out one bit at a time.
///
/// Writing to the chip picks a channel *and* latches that channel's reading
/// into the shift register in one action; each read then hands back the top
/// bit and shifts, with zeros feeding in behind. Latching lazily on the first
/// read instead -- which is what this did -- leaves the register holding
/// whatever the previous conversion had shifted down to, and the wheel reads
/// hard over.
#[derive(Default)]
struct Adc {
    shifter: Cell<u8>,
}

impl Adc {
    fn latch(&self, value: u8) {
        self.shifter.set(value);
    }

    /// The next bit of the conversion, most significant first, in bit 0.
    fn shift_out(&self) -> u8 {
        let shifter = self.shifter.get();
        self.shifter.set(shifter << 1);
        shifter >> 7
    }
}
