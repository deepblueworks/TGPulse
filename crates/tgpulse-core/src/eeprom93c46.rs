//! 93C46 serial EEPROM, 64 x 16 bits.
//!
//! On Model 2A/2B/2C the chip hangs off the 315-5649 I/O chip rather than the
//! bus: port A drives chip select, clock and data-in, and the data-out line
//! comes back in bit 5 of port B. The games bit-bang it a clock edge at a time,
//! so the device has to be a real state machine -- returning a constant from
//! the data line makes every word read back as 0xFFFF, which Wave Runner treats
//! as a corrupt scroll table and answers with "SCROLL GROUP ERROR !!".
//!
//!cpp`).

const ADDRESS_BITS: u32 = 6;
const DATA_BITS: u32 = 16;
const WORDS: usize = 1 << ADDRESS_BITS;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
enum State {
    InReset,
    WaitForStartBit,
    WaitForCommand,
    ReadingData,
    WaitForData,
    WaitForCompletion,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
enum Command {
    Invalid,
    Read,
    Write,
    Erase,
    Lock,
    Unlock,
    WriteAll,
    EraseAll,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Eeprom93c46 {
    #[serde(with = "serde_big_array::BigArray")]
    pub data: [u16; WORDS],
    state: State,
    command: Command,
    address: u32,
    /// Command and address bits as they clock in, MSB first.
    cmd_accum: u32,
    /// Read shift register; the output line is its bit 31.
    shift: u32,
    bits: u32,
    locked: bool,
    cs: bool,
    clk: bool,
    di: bool,
    /// Set when the host has written a word, so the caller can persist it.
    pub dirty: bool,
}

impl Default for Eeprom93c46 {
    fn default() -> Self {
        Self::new()
    }
}

impl Eeprom93c46 {
    /// A blank chip reads back as all ones, which is what an unprogrammed
    /// part and the reference NVRAM both give.
    pub fn new() -> Self {
        Self {
            data: [0xFFFF; WORDS],
            state: State::InReset,
            command: Command::Invalid,
            address: 0,
            cmd_accum: 0,
            shift: 0,
            bits: 0,
            locked: true,
            cs: false,
            clk: false,
            di: false,
            dirty: false,
        }
    }

    /// The DO line, as port B bit 5 samples it.
    pub fn do_read(&self) -> bool {
        // Outside a read the line floats high, which is how the games see an
        // idle chip.
        match self.state {
            State::ReadingData => self.shift & 0x8000_0000 != 0,
            _ => true,
        }
    }

    pub fn cs_write(&mut self, level: bool) {
        if level == self.cs {
            return;
        }
        self.cs = level;
        if level {
            if self.state == State::InReset {
                self.state = State::WaitForStartBit;
            }
        } else {
            self.state = State::InReset;
        }
    }

    pub fn di_write(&mut self, level: bool) {
        self.di = level;
    }

    pub fn clk_write(&mut self, level: bool) {
        let rising = level && !self.clk;
        self.clk = level;
        if !rising {
            return;
        }
        match self.state {
            State::WaitForStartBit => {
                if self.di {
                    self.cmd_accum = 0;
                    self.bits = 0;
                    self.state = State::WaitForCommand;
                }
            }
            State::WaitForCommand => {
                self.cmd_accum = (self.cmd_accum << 1) | self.di as u32;
                self.bits += 1;
                if self.bits == 2 + ADDRESS_BITS {
                    self.execute_command();
                }
            }
            State::ReadingData => {
                // The first clock after the command fetches the word; the rest
                // shift it out. Bit 31 is the line, so the word sits at the top.
                if self.bits == 0 {
                    self.shift =
                        (self.data[self.address as usize % WORDS] as u32) << (32 - DATA_BITS);
                } else {
                    self.shift = (self.shift << 1) | 1;
                }
                self.bits += 1;
            }
            State::WaitForData => {
                self.shift = (self.shift << 1) | self.di as u32;
                self.bits += 1;
                if self.bits == DATA_BITS {
                    self.execute_write();
                }
            }
            State::InReset | State::WaitForCompletion => {}
        }
    }

    fn execute_command(&mut self) {
        let mask = (1u32 << ADDRESS_BITS) - 1;
        self.address = self.cmd_accum & mask;
        self.command = match self.cmd_accum >> ADDRESS_BITS {
            1 => Command::Write,
            2 => Command::Read,
            3 => Command::Erase,
            // Opcode 0 puts the real operation in the top two address bits.
            _ => {
                let sub = self.address >> (ADDRESS_BITS - 2);
                self.address = 0;
                match sub {
                    0 => Command::Lock,
                    1 => Command::WriteAll,
                    2 => Command::EraseAll,
                    _ => Command::Unlock,
                }
            }
        };
        self.bits = 0;

        match self.command {
            Command::Read => {
                // Zeroed so the first clock presents the dummy 0 bit that the
                // real part emits before the data.
                self.shift = 0;
                self.state = State::ReadingData;
            }
            Command::Write | Command::WriteAll => {
                self.shift = 0;
                self.state = State::WaitForData;
            }
            Command::Erase => {
                if !self.locked {
                    self.data[self.address as usize % WORDS] = 0xFFFF;
                    self.dirty = true;
                    self.state = State::WaitForCompletion;
                } else {
                    self.state = State::InReset;
                }
            }
            Command::EraseAll => {
                if !self.locked {
                    self.data = [0xFFFF; WORDS];
                    self.dirty = true;
                    self.state = State::WaitForCompletion;
                } else {
                    self.state = State::InReset;
                }
            }
            Command::Lock => {
                self.locked = true;
                self.state = State::InReset;
            }
            Command::Unlock => {
                self.locked = false;
                self.state = State::InReset;
            }
            Command::Invalid => self.state = State::InReset,
        }
    }

    fn execute_write(&mut self) {
        let value = self.shift as u16;
        if !self.locked {
            match self.command {
                Command::WriteAll => self.data = [value; WORDS],
                _ => self.data[self.address as usize % WORDS] = value,
            }
            self.dirty = true;
        }
        self.bits = 0;
        self.state = State::WaitForCompletion;
    }
}
