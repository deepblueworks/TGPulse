//! license:BSD-3-Clause
//! copyright-holders:Andreas Naive, Olivier Galibert, David Haywood
//!
//! Sega 315-5881 ROM decryption.
//!
//! The index-based loops and the wide argument lists are deliberate: this
//! cipher is only checkable by reading an implementation against the algorithm
//! step by step, so the shape is worth more here than the idiom.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

use std::mem;

/// Decrypts the entire program ROM in-place using the 315-5881 algorithm.
///
/// # Arguments
/// * `data` - Mutable slice of the encrypted ROM data (as 32-bit words).
/// * `key` - The unique game key (e.g., 0x042c0d13 for Daytona USA).
pub fn decrypt_rom(data: &mut [u32], key: u32) {
    let mut crypt = Sega315_5881Crypt::new(key);

    // We create a read-only copy of the encrypted data to act as the "bus"
    // that the decryption device reads from while we write to the mutable slice.
    let source_data = data.to_vec();

    // 1. Reset the device address to 0
    crypt.addrlo_w(0);
    crypt.addrhi_w(0);

    // 2. Define the "Bus Read" closure
    // The device requests 16-bit words at specific byte addresses.
    // We map these requests to our 32-bit ROM vector.
    let mut reader = |addr: u32| -> u16 {
        // Model 2 is Little Endian.
        // Address 0 = low 16 bits of word 0.
        // Address 2 = high 16 bits of word 0.
        let word_idx = (addr / 4) as usize;

        if word_idx < source_data.len() {
            let dword = source_data[word_idx];
            if (addr & 2) == 0 {
                (dword & 0xFFFF) as u16 // Lower 16 bits
            } else {
                ((dword >> 16) & 0xFFFF) as u16 // Upper 16 bits
            }
        } else {
            0
        }
    };

    // 3. Clear the "First Read" latency artifact
    // The real hardware (and this emulation) returns 0/garbage on the very first read
    // after an address set. We consume that here.
    let _ = crypt.decrypt_le_r(&mut reader);

    log::info!(target: "crypt", "decrypting {} words with key {key:08X}", data.len());

    // 4. Decrypt the Stream
    // We iterate through the mutable array, fetching decrypted words 16 bits at a time
    // and reassembling them into 32-bit words.
    for i in 0..data.len() {
        let low = crypt.decrypt_le_r(&mut reader) as u32;
        let high = crypt.decrypt_le_r(&mut reader) as u32;

        data[i] = low | (high << 16);
    }
}

const BUFFER_SIZE: usize = 2;
const LINE_SIZE: usize = 512;
const FLAG_COMPRESSED: u32 = 0x20000;

// Constants for S-box and Key Scheduling sizes
const FN1GK: usize = 38;
const FN2GK: usize = 32;

/// Represents the table logic for S-boxes
struct SBox {
    table: [u8; 64],
    inputs: [i8; 6], // Using i8 to allow -1 sentinel
    outputs: [u8; 2],
}

pub struct Sega315_5881Crypt {
    key: u32,

    // Buffers
    buffer: Vec<u8>,
    line_buffer: Vec<u8>,
    line_buffer_prev: Vec<u8>,

    // Registers
    prot_cur_address: u32,
    subkey: u16,
    dec_hist: u16,
    dec_header: u32,

    // State flags
    enc_ready: bool,
    first_read: bool,

    // Counters and offsets
    buffer_pos: usize,
    line_buffer_pos: usize,
    line_buffer_size: usize,
    buffer_bit: i8,
    buffer_bit2: i8, // Using i8 to match logic decrementing to -1

    buffer2: [u8; 2],
    buffer2a: u16,

    block_size: usize,
    block_pos: usize,
    block_numlines: usize,
    done_compression: i32,
}

impl Sega315_5881Crypt {
    pub fn new(key: u32) -> Self {
        let mut device = Self {
            key,
            buffer: vec![0; BUFFER_SIZE],
            line_buffer: vec![0; LINE_SIZE],
            line_buffer_prev: vec![0; LINE_SIZE],
            prot_cur_address: 0,
            subkey: 0,
            dec_hist: 0,
            dec_header: 0,
            enc_ready: false,
            first_read: false,
            buffer_pos: 0,
            line_buffer_pos: 0,
            line_buffer_size: 0,
            buffer_bit: 0,
            buffer_bit2: 0,
            buffer2: [0; 2],
            buffer2a: 0,
            block_size: 0,
            block_pos: 0,
            block_numlines: 0,
            done_compression: 0,
        };
        device.reset();
        device
    }

    pub fn reset(&mut self) {
        self.buffer.fill(0);
        self.line_buffer.fill(0);
        self.line_buffer_prev.fill(0);

        self.prot_cur_address = 0;
        self.subkey = 0;
        self.dec_hist = 0;
        self.dec_header = 0;
        self.enc_ready = false;
        self.first_read = false;

        self.buffer_pos = 0;
        self.line_buffer_pos = 0;
        self.line_buffer_size = 0;
        self.buffer_bit = 0;
        self.buffer_bit2 = 0;

        self.block_pos = 0;
        self.done_compression = 0;
    }

    // --- Interface Methods ---

    pub fn addrlo_w(&mut self, data: u16) {
        self.set_addr_low(data);
        self.first_read = true;
    }

    pub fn addrhi_w(&mut self, data: u16) {
        self.set_addr_high(0);
        if data != 0 {
            log::warn!(target: "crypt", "non-zero high address {data:08X}");
        }
        self.first_read = true;
    }

    /// Read decrypted data (Little Endian)
    pub fn decrypt_le_r<F>(&mut self, reader: &mut F) -> u16
    where
        F: FnMut(u32) -> u16,
    {
        let mut retval = self.decrypt_be_r(reader);
        // endian swap for little endian CPUs
        retval = ((retval & 0xff00) >> 8) | ((retval & 0x00ff) << 8);
        retval
    }

    /// Read decrypted data (Big Endian)
    pub fn decrypt_be_r<F>(&mut self, reader: &mut F) -> u16
    where
        F: FnMut(u32) -> u16,
    {
        if self.first_read {
            self.first_read = false;
            return 0;
        }

        self.do_decrypt(reader)
    }

    // --- Internal Logic ---
    fn do_decrypt<F>(&mut self, reader: &mut F) -> u16
    where
        F: FnMut(u32) -> u16,
    {
        if !self.enc_ready {
            self.enc_start(reader);
        }

        let val_high: u8;
        let val_low: u8;

        if (self.dec_header & FLAG_COMPRESSED) != 0 {
            // --- Compressed Mode ---

            // 1. Read High Byte
            if self.line_buffer_pos >= self.line_buffer_size {
                if self.done_compression == 1 {
                    self.enc_start(reader);
                }
                self.line_fill(reader);
            }
            val_high = self.line_buffer[self.line_buffer_pos];
            self.line_buffer_pos += 1;

            // 2. Read Low Byte (Check for boundary AGAIN!)
            if self.line_buffer_pos >= self.line_buffer_size {
                if self.done_compression == 1 {
                    self.enc_start(reader);
                }
                self.line_fill(reader);
            }
            val_low = self.line_buffer[self.line_buffer_pos];
            self.line_buffer_pos += 1;
        } else {
            // --- Uncompressed Mode ---

            // 1. Read High Byte
            if self.buffer_pos == BUFFER_SIZE {
                self.enc_fill(reader);
            }
            val_high = self.buffer[self.buffer_pos];
            self.buffer_pos += 1;

            // 2. Read Low Byte
            if self.buffer_pos == BUFFER_SIZE {
                self.enc_fill(reader);
            }
            val_low = self.buffer[self.buffer_pos];
            self.buffer_pos += 1;
        }

        ((val_high as u16) << 8) | (val_low as u16)
    }

    fn set_addr_low(&mut self, data: u16) {
        self.prot_cur_address = (self.prot_cur_address & 0xffff0000) | (data as u32);
        self.enc_ready = false;
    }

    fn set_addr_high(&mut self, data: u16) {
        self.prot_cur_address = (self.prot_cur_address & 0x0000ffff) | ((data as u32) << 16);
        self.enc_ready = false;
        self.buffer_bit = 7;
        self.buffer_bit2 = 15;
    }

    pub fn set_subkey(&mut self, data: u16) {
        self.subkey = data;
        self.enc_ready = false;
    }

    fn enc_start<F>(&mut self, reader: &mut F)
    where
        F: FnMut(u32) -> u16,
    {
        self.block_pos = 0;
        self.done_compression = 0;
        self.buffer_pos = BUFFER_SIZE;

        if self.buffer_bit2 < 14 {
            // Use existing bits in buffer
            self.dec_header = ((self.buffer2a & 0x0003) as u32) << 16;
        } else {
            self.dec_hist = 0;
            self.dec_header = (self.get_decrypted_16(reader) as u32) << 16;
        }

        self.dec_header |= self.get_decrypted_16(reader) as u32;

        self.block_numlines = ((self.dec_header & 0x000000ff) as usize) + 1;
        let blocky = (((self.dec_header & 0x0001ff00) >> 8) as usize) + 1;
        self.block_size = self.block_numlines * blocky;

        if (self.dec_header & FLAG_COMPRESSED) != 0 {
            self.line_buffer_size = blocky;
            self.line_buffer_pos = self.line_buffer_size;
            self.buffer_bit = 7;
            self.buffer_bit2 = 15;
        }

        self.enc_ready = true;
    }

    fn enc_fill<F>(&mut self, reader: &mut F)
    where
        F: FnMut(u32) -> u16,
    {
        assert_eq!(self.buffer_pos, BUFFER_SIZE);

        let mut i = 0;
        while i < BUFFER_SIZE {
            let val = self.get_decrypted_16(reader);
            self.buffer[i] = (val & 0xff) as u8;
            self.buffer[i + 1] = (val >> 8) as u8;
            self.block_pos += 2;

            if (self.dec_header & FLAG_COMPRESSED) == 0 && self.block_pos == self.block_size {
                self.enc_start(reader);
            }
            i += 2;
        }
        self.buffer_pos = 0;
    }

    fn line_fill<F>(&mut self, reader: &mut F)
    where
        F: FnMut(u32) -> u16,
    {
        assert_eq!(self.line_buffer_pos, self.line_buffer_size);

        // Swap buffers
        mem::swap(&mut self.line_buffer, &mut self.line_buffer_prev);

        self.line_buffer_pos = 0;
        let mut i = 0;

        while i < self.line_buffer_size {
            // vlc 0: start of line, 1: interior, 2-9: near end
            let slot = if i > 0 {
                if i < self.line_buffer_size - 7 {
                    1
                } else {
                    (i & 7) + 1
                }
            } else {
                0
            };

            let mut tmp: u32 = 0;
            while (tmp & 0x80) == 0 {
                if self.get_compressed_bit(reader) != 0 {
                    tmp = TREES[slot][1][tmp as usize] as u32;
                } else {
                    tmp = TREES[slot][0][tmp as usize] as u32;
                }
            }

            if tmp != 0xff {
                let count = (tmp & 7) + 1;

                if (tmp & 0x40) != 0 {
                    // Copy from previous line
                    let offsets: [isize; 4] = [0, 1, 0, -1];
                    let offset = offsets[((tmp & 0x18) >> 3) as usize];

                    for _ in 0..count {
                        // Logic relies on byte XOR flipping for endianness adjustment during copy
                        // i^1 gives us the "byte neighbor"
                        let src_idx = (i as isize + offset)
                            .rem_euclid(self.line_buffer_size as isize)
                            as usize;
                        self.line_buffer[i ^ 1] = self.line_buffer_prev[src_idx ^ 1];
                        i += 1;
                    }
                } else {
                    // Get a byte in stream
                    let mut byte: u8;
                    byte = (self.get_compressed_bit(reader) << 1) as u8;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte = (byte | self.get_compressed_bit(reader) as u8) << 1;
                    byte |= self.get_compressed_bit(reader) as u8;

                    for _ in 0..count {
                        self.line_buffer[i ^ 1] = byte;
                        i += 1;
                    }
                }
            }
        }

        self.block_pos += 1;
        if self.block_numlines == self.block_pos {
            self.done_compression = 1;
        }
    }

    fn get_compressed_bit<F>(&mut self, reader: &mut F) -> i32
    where
        F: FnMut(u32) -> u16,
    {
        if self.buffer_bit2 == 15 {
            self.buffer_bit2 = 0;
            self.buffer2a = self.get_decrypted_16(reader);
            self.buffer2[0] = (self.buffer2a & 0xff) as u8;
            self.buffer2[1] = (self.buffer2a >> 8) as u8;
            self.buffer_pos = 0;
        } else {
            self.buffer_bit2 += 1;
        }

        let idx = (self.buffer_pos & 1) ^ 1;
        let res = (self.buffer2[idx] >> self.buffer_bit) & 1;

        self.buffer_bit -= 1;
        if self.buffer_bit == -1 {
            self.buffer_bit = 7;
            self.buffer_pos += 1;
        }
        res as i32
    }

    fn get_decrypted_16<F>(&mut self, reader: &mut F) -> u16
    where
        F: FnMut(u32) -> u16,
    {
        // Read encrypted from Bus
        let enc = reader(self.prot_cur_address);

        let dec = self.block_decrypt(self.key, self.subkey, self.prot_cur_address as u16, enc);
        let res = (dec & 3) | (self.dec_hist & 0xfffc);
        self.dec_hist = dec;

        self.prot_cur_address += 1;
        res
    }

    // --- Crypto Core ---

    fn block_decrypt(&self, game_key: u32, sequence_key: u16, counter: u16, data: u16) -> u16 {
        let mut fn1_subkeys = [0u32; 4];
        let mut fn2_subkeys = [0u32; 4];

        // 1. Game Key Scheduling
        for j in 0..FN1GK {
            if bit(game_key, FN1_GAME_KEY_SCHEDULING[j][0]) != 0 {
                let val = FN1_GAME_KEY_SCHEDULING[j][1];
                let aux = val % 24;
                let aux2 = val / 24;
                fn1_subkeys[aux2 as usize] ^= 1 << aux;
            }
        }
        for j in 0..FN2GK {
            if bit(game_key, FN2_GAME_KEY_SCHEDULING[j][0]) != 0 {
                let val = FN2_GAME_KEY_SCHEDULING[j][1];
                let aux = val % 24;
                let aux2 = val / 24;
                fn2_subkeys[aux2 as usize] ^= 1 << aux;
            }
        }

        // 2. Sequence Key Scheduling
        for j in 0..20 {
            if bit(sequence_key as u32, FN1_SEQUENCE_KEY_SCHEDULING[j][0]) != 0 {
                let val = FN1_SEQUENCE_KEY_SCHEDULING[j][1];
                let aux = val % 24;
                let aux2 = val / 24;
                fn1_subkeys[aux2 as usize] ^= 1 << aux;
            }
        }
        for j in 0..16 {
            if bit(sequence_key as u32, j as u32) != 0 {
                let val = FN2_SEQUENCE_KEY_SCHEDULING[j];
                let aux = val % 24;
                let aux2 = val / 24;
                fn2_subkeys[aux2 as usize] ^= 1 << aux;
            }
        }

        // 3. First Feistel Network
        let mut aux = bitswap16(
            counter, 5, 12, 14, 13, 9, 3, 6, 4, 8, 1, 15, 11, 0, 7, 10, 2,
        );

        let mut b = aux >> 8;
        let mut a = (aux & 0xff) ^ Self::feistel_function(b as i32, &FN1_SBOXES[0], fn1_subkeys[0]);
        b ^= Self::feistel_function(a as i32, &FN1_SBOXES[1], fn1_subkeys[1]);
        a ^= Self::feistel_function(b as i32, &FN1_SBOXES[2], fn1_subkeys[2]);
        b ^= Self::feistel_function(a as i32, &FN1_SBOXES[3], fn1_subkeys[3]);

        let middle_result = (b << 8) | a;

        // 4. Middle Result Key Scheduling
        for j in 0..16 {
            if bit(middle_result as u32, j as u32) != 0 {
                let val = FN2_MIDDLE_RESULT_SCHEDULING[j];
                let aux = val % 24;
                let aux2 = val / 24;
                fn2_subkeys[aux2 as usize] ^= 1 << aux;
            }
        }

        // 5. Second Feistel Network
        aux = bitswap16(data, 14, 3, 8, 12, 13, 7, 15, 4, 6, 2, 9, 5, 11, 0, 1, 10);

        b = aux >> 8;
        a = (aux & 0xff) ^ Self::feistel_function(b as i32, &FN2_SBOXES[0], fn2_subkeys[0]);
        b ^= Self::feistel_function(a as i32, &FN2_SBOXES[1], fn2_subkeys[1]);
        a ^= Self::feistel_function(b as i32, &FN2_SBOXES[2], fn2_subkeys[2]);
        b ^= Self::feistel_function(a as i32, &FN2_SBOXES[3], fn2_subkeys[3]);

        aux = (b << 8) | a;

        bitswap16(aux, 15, 7, 6, 14, 13, 12, 5, 4, 3, 2, 11, 10, 9, 1, 0, 8)
    }

    fn feistel_function(input: i32, sboxes: &[SBox; 4], mut subkeys: u32) -> u16 {
        let mut result = 0;

        for m in 0..4 {
            let mut aux = 0;
            for k in 0..6 {
                let input_idx = sboxes[m].inputs[k];
                if input_idx != -1 {
                    aux |= bit(input as u32, input_idx as u32) << k;
                }
            }

            // XOR with subkey part and mask to 6 bits
            aux = sboxes[m].table[((aux ^ subkeys) & 0x3f) as usize] as u32;

            for k in 0..2 {
                result |= bit(aux, k as u32) << sboxes[m].outputs[k];
            }

            subkeys >>= 6;
        }
        result as u16
    }
}

// --- Helper Functions ---

fn bit(val: u32, n: u32) -> u32 {
    (val >> n) & 1
}

fn bitswap16(
    val: u16,
    b15: u32,
    b14: u32,
    b13: u32,
    b12: u32,
    b11: u32,
    b10: u32,
    b9: u32,
    b8: u32,
    b7: u32,
    b6: u32,
    b5: u32,
    b4: u32,
    b3: u32,
    b2: u32,
    b1: u32,
    b0: u32,
) -> u16 {
    let mut res = 0;
    res |= bit(val as u32, b15) << 15;
    res |= bit(val as u32, b14) << 14;
    res |= bit(val as u32, b13) << 13;
    res |= bit(val as u32, b12) << 12;
    res |= bit(val as u32, b11) << 11;
    res |= bit(val as u32, b10) << 10;
    res |= bit(val as u32, b9) << 9;
    res |= bit(val as u32, b8) << 8;
    res |= bit(val as u32, b7) << 7;
    res |= bit(val as u32, b6) << 6;
    res |= bit(val as u32, b5) << 5;
    res |= bit(val as u32, b4) << 4;
    res |= bit(val as u32, b3) << 3;
    res |= bit(val as u32, b2) << 2;
    res |= bit(val as u32, b1) << 1;
    res |= bit(val as u32, b0);
    res as u16
}

// --- Static Data ---

static FN1_GAME_KEY_SCHEDULING: [[u32; 2]; FN1GK] = [
    [1, 29],
    [1, 71],
    [2, 4],
    [2, 54],
    [3, 8],
    [4, 56],
    [4, 73],
    [5, 11],
    [6, 51],
    [7, 92],
    [8, 89],
    [9, 9],
    [9, 39],
    [9, 58],
    [10, 90],
    [11, 6],
    [12, 64],
    [13, 49],
    [14, 44],
    [15, 40],
    [16, 69],
    [17, 15],
    [18, 23],
    [18, 43],
    [19, 82],
    [20, 81],
    [21, 32],
    [22, 5],
    [23, 66],
    [24, 13],
    [24, 45],
    [25, 12],
    [25, 35],
    [26, 61],
    [27, 10],
    [27, 59],
    [28, 25],
    [29, 86],
];

static FN2_GAME_KEY_SCHEDULING: [[u32; 2]; FN2GK] = [
    [0, 0],
    [1, 3],
    [2, 11],
    [3, 20],
    [4, 22],
    [5, 23],
    [6, 29],
    [7, 38],
    [8, 39],
    [9, 55],
    [9, 86],
    [9, 87],
    [10, 50],
    [11, 57],
    [12, 59],
    [13, 61],
    [14, 63],
    [15, 67],
    [16, 72],
    [17, 83],
    [18, 88],
    [19, 94],
    [20, 35],
    [21, 17],
    [22, 6],
    [23, 85],
    [24, 16],
    [25, 25],
    [26, 92],
    [27, 47],
    [28, 28],
    [29, 90],
];

static FN1_SEQUENCE_KEY_SCHEDULING: [[u32; 2]; 20] = [
    [0, 52],
    [1, 34],
    [2, 17],
    [3, 36],
    [4, 84],
    [4, 88],
    [5, 57],
    [6, 48],
    [6, 68],
    [7, 76],
    [8, 83],
    [9, 30],
    [10, 22],
    [10, 41],
    [11, 38],
    [12, 55],
    [13, 74],
    [14, 19],
    [14, 80],
    [15, 26],
];

static FN2_SEQUENCE_KEY_SCHEDULING: [u32; 16] =
    [77, 34, 8, 42, 36, 27, 69, 66, 13, 9, 79, 31, 49, 7, 24, 64];

static FN2_MIDDLE_RESULT_SCHEDULING: [u32; 16] =
    [1, 10, 44, 68, 74, 78, 81, 95, 2, 4, 30, 40, 41, 51, 53, 58];

static TREES: [[[u8; 32]; 2]; 9] = [
    [
        [
            0x01, 0x10, 0x0f, 0x05, 0xc4, 0x13, 0x87, 0x0a, 0xcc, 0x81, 0xce, 0x0c, 0x86, 0x0e,
            0x84, 0xc2, 0x11, 0xc1, 0xc3, 0xcf, 0x15, 0xc8, 0xcd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc7, 0x02, 0x03, 0x04, 0x80, 0x06, 0x07, 0x08, 0x09, 0xc9, 0x0b, 0x0d, 0x82, 0x83,
            0x85, 0xc0, 0x12, 0xc6, 0xc5, 0x14, 0x16, 0xca, 0xcb, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x02, 0x80, 0x05, 0x04, 0x81, 0x10, 0x15, 0x82, 0x09, 0x83, 0x0b, 0x0c, 0x0d, 0xdc,
            0x0f, 0xde, 0x1c, 0xcf, 0xc5, 0xdd, 0x86, 0x16, 0x87, 0x18, 0x19, 0x1a, 0xda, 0xca,
            0xc9, 0x1e, 0xce, 0xff,
        ],
        [
            0x01, 0x17, 0x03, 0x0a, 0x08, 0x06, 0x07, 0xc2, 0xd9, 0xc4, 0xd8, 0xc8, 0x0e, 0x84,
            0xcb, 0x85, 0x11, 0x12, 0x13, 0x14, 0xcd, 0x1b, 0xdb, 0xc7, 0xc0, 0xc1, 0x1d, 0xdf,
            0xc3, 0xc6, 0xcc, 0xff,
        ],
    ],
    [
        [
            0xc6, 0x80, 0x03, 0x0b, 0x05, 0x07, 0x82, 0x08, 0x15, 0xdc, 0xdd, 0x0c, 0xd9, 0xc2,
            0x14, 0x10, 0x85, 0x86, 0x18, 0x16, 0xc5, 0xc4, 0xc8, 0xc9, 0xc0, 0xcc, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0x01, 0x02, 0x12, 0x04, 0x81, 0x06, 0x83, 0xc3, 0x09, 0x0a, 0x84, 0x11, 0x0d, 0x0e,
            0x0f, 0x19, 0xca, 0xc1, 0x13, 0xd8, 0xda, 0xdb, 0x17, 0xde, 0xcd, 0xcb, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0x80, 0x0d, 0x04, 0x05, 0x15, 0x83, 0x08, 0xd9, 0x10, 0x0b, 0x0c, 0x84, 0x0e,
            0xc0, 0x14, 0x12, 0xcb, 0x13, 0xca, 0xc8, 0xc2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc5, 0x02, 0x03, 0x07, 0x81, 0x06, 0x82, 0xcc, 0x09, 0x0a, 0xc9, 0x11, 0xc4, 0x0f,
            0x85, 0xd8, 0xda, 0xdb, 0xc3, 0xdc, 0xdd, 0xc1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0x80, 0x06, 0x0c, 0x05, 0x81, 0xd8, 0x84, 0x09, 0xdc, 0x0b, 0x0f, 0x0d, 0x0e,
            0x10, 0xdb, 0x11, 0xca, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc4, 0x02, 0x03, 0x04, 0xcb, 0x0a, 0x07, 0x08, 0xd9, 0x82, 0xc8, 0x83, 0xc0, 0xc1,
            0xda, 0xc2, 0xc9, 0xc3, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0x02, 0x06, 0x0a, 0x83, 0x0b, 0x07, 0x08, 0x09, 0x82, 0xd8, 0x0c, 0xd9, 0xda,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc3, 0x80, 0x03, 0x04, 0x05, 0x81, 0xca, 0xc8, 0xdb, 0xc9, 0xc0, 0xc1, 0x0d, 0xc2,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0x02, 0x03, 0x04, 0x81, 0x07, 0x08, 0xd8, 0xda, 0xd9, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc2, 0x80, 0x05, 0xc9, 0xc8, 0x06, 0x82, 0xc0, 0x09, 0xc1, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0x80, 0x04, 0xc8, 0xc0, 0xd9, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc1, 0x02, 0x03, 0x81, 0x05, 0xd8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
    [
        [
            0x01, 0xd8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
        [
            0xc0, 0x80, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ],
    ],
];

static FN1_SBOXES: [[SBox; 4]; 4] = [
    [
        // 1st round
        SBox {
            table: [
                0, 3, 2, 2, 1, 3, 1, 2, 3, 2, 1, 2, 1, 2, 3, 1, 3, 2, 2, 0, 2, 1, 3, 0, 0, 3, 2, 3,
                2, 1, 2, 0, 2, 3, 1, 1, 2, 2, 1, 1, 1, 0, 2, 3, 3, 0, 2, 1, 1, 1, 1, 1, 3, 0, 3, 2,
                1, 0, 1, 2, 0, 3, 1, 3,
            ],
            inputs: [3, 4, 5, 7, -1, -1],
            outputs: [0, 4],
        },
        SBox {
            table: [
                2, 2, 2, 0, 3, 3, 0, 1, 2, 2, 3, 2, 3, 0, 2, 2, 1, 1, 0, 3, 3, 2, 0, 2, 0, 1, 0, 1,
                2, 3, 1, 1, 0, 1, 3, 3, 1, 3, 3, 1, 2, 3, 2, 0, 0, 0, 2, 2, 0, 3, 1, 3, 0, 3, 2, 2,
                0, 3, 0, 3, 1, 1, 0, 2,
            ],
            inputs: [0, 1, 2, 5, 6, 7],
            outputs: [1, 6],
        },
        SBox {
            table: [
                0, 1, 3, 0, 3, 1, 1, 1, 1, 2, 3, 1, 3, 0, 2, 3, 3, 2, 0, 2, 1, 1, 2, 1, 1, 3, 1, 0,
                0, 2, 0, 1, 1, 3, 1, 0, 0, 3, 2, 3, 2, 0, 3, 3, 0, 0, 0, 0, 1, 2, 3, 3, 2, 0, 3, 2,
                1, 0, 0, 0, 2, 2, 3, 3,
            ],
            inputs: [0, 2, 5, 6, 7, -1],
            outputs: [2, 3],
        },
        SBox {
            table: [
                3, 2, 1, 2, 1, 2, 3, 2, 0, 3, 2, 2, 3, 1, 3, 3, 0, 2, 3, 0, 3, 3, 2, 1, 1, 1, 2, 0,
                2, 2, 0, 1, 1, 3, 3, 0, 0, 3, 0, 3, 0, 2, 1, 3, 2, 1, 0, 0, 0, 1, 1, 2, 0, 1, 0, 0,
                0, 1, 3, 3, 2, 0, 3, 3,
            ],
            inputs: [1, 2, 3, 4, 6, 7],
            outputs: [5, 7],
        },
    ],
    [
        // 2nd round
        SBox {
            table: [
                3, 3, 1, 2, 0, 0, 2, 2, 2, 1, 2, 1, 3, 1, 1, 3, 3, 0, 0, 3, 0, 3, 3, 2, 1, 1, 3, 2,
                3, 2, 1, 3, 2, 3, 0, 1, 3, 2, 0, 1, 2, 1, 3, 1, 2, 2, 3, 3, 3, 1, 2, 2, 0, 3, 1, 2,
                2, 1, 3, 0, 3, 0, 1, 3,
            ],
            inputs: [0, 1, 3, 4, 5, 7],
            outputs: [0, 4],
        },
        SBox {
            table: [
                2, 0, 1, 0, 0, 3, 2, 0, 3, 3, 1, 2, 1, 3, 0, 2, 0, 2, 0, 0, 0, 2, 3, 1, 3, 1, 1, 2,
                3, 0, 3, 0, 3, 0, 2, 0, 0, 2, 2, 1, 0, 2, 3, 3, 1, 3, 1, 0, 1, 3, 3, 0, 0, 1, 3, 1,
                0, 2, 0, 3, 2, 1, 0, 1,
            ],
            inputs: [0, 1, 3, 4, 6, -1],
            outputs: [1, 5],
        },
        SBox {
            table: [
                2, 2, 2, 3, 1, 1, 0, 1, 3, 3, 1, 1, 2, 2, 2, 0, 0, 3, 2, 3, 3, 0, 2, 1, 2, 2, 3, 0,
                1, 3, 0, 0, 3, 2, 0, 3, 2, 0, 1, 0, 0, 1, 2, 2, 3, 3, 0, 2, 2, 1, 3, 1, 1, 1, 1, 2,
                0, 3, 1, 0, 0, 2, 3, 2,
            ],
            inputs: [1, 2, 5, 6, 7, 6],
            outputs: [2, 7],
        },
        SBox {
            table: [
                0, 1, 3, 3, 3, 1, 3, 3, 1, 0, 2, 0, 2, 0, 0, 3, 1, 2, 1, 3, 1, 2, 3, 2, 2, 0, 1, 3,
                0, 3, 3, 3, 0, 0, 0, 2, 1, 1, 2, 3, 2, 2, 3, 1, 1, 2, 0, 2, 0, 2, 1, 3, 1, 1, 3, 3,
                1, 1, 3, 0, 2, 3, 0, 0,
            ],
            inputs: [2, 3, 4, 5, 6, 7],
            outputs: [3, 6],
        },
    ],
    [
        // 3rd round
        SBox {
            table: [
                0, 0, 1, 0, 1, 0, 0, 3, 2, 0, 0, 3, 0, 1, 0, 2, 0, 3, 0, 0, 2, 0, 3, 2, 2, 1, 3, 2,
                2, 1, 1, 2, 0, 0, 0, 3, 0, 1, 1, 0, 0, 2, 1, 0, 3, 1, 2, 2, 2, 0, 3, 1, 3, 0, 1, 2,
                2, 1, 1, 1, 0, 2, 3, 1,
            ],
            inputs: [1, 2, 3, 4, 5, 7],
            outputs: [0, 5],
        },
        SBox {
            table: [
                1, 2, 1, 0, 3, 1, 1, 2, 0, 0, 2, 3, 2, 3, 1, 3, 2, 0, 3, 2, 2, 3, 1, 1, 1, 1, 0, 3,
                2, 0, 0, 1, 1, 0, 0, 1, 3, 1, 2, 3, 0, 0, 2, 3, 3, 0, 1, 0, 0, 2, 3, 0, 1, 2, 0, 1,
                3, 3, 3, 1, 2, 0, 2, 1,
            ],
            inputs: [0, 2, 4, 5, 6, 7],
            outputs: [1, 6],
        },
        SBox {
            table: [
                0, 3, 0, 2, 1, 2, 0, 0, 1, 1, 0, 0, 3, 1, 1, 0, 0, 3, 0, 0, 2, 3, 3, 2, 3, 1, 2, 0,
                0, 2, 3, 0, // unused?
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            inputs: [0, 2, 4, 6, 7, -1],
            outputs: [2, 3],
        },
        SBox {
            table: [
                0, 0, 1, 0, 0, 1, 0, 2, 3, 3, 0, 3, 3, 2, 3, 0, 2, 2, 2, 0, 3, 2, 0, 3, 1, 0, 0, 3,
                3, 0, 0, 0, 2, 2, 1, 0, 2, 0, 3, 2, 0, 0, 3, 1, 3, 3, 0, 0, 2, 1, 1, 2, 1, 0, 1, 1,
                0, 3, 1, 2, 0, 2, 0, 3,
            ],
            inputs: [0, 1, 2, 3, 6, -1],
            outputs: [4, 7],
        },
    ],
    [
        // 4th round
        SBox {
            table: [
                0, 3, 3, 3, 3, 3, 2, 0, 0, 1, 2, 0, 2, 2, 2, 2, 1, 1, 0, 2, 2, 1, 3, 2, 3, 2, 0, 1,
                2, 3, 2, 1, 3, 2, 2, 3, 1, 0, 1, 0, 0, 2, 0, 1, 2, 1, 2, 3, 1, 2, 1, 1, 2, 2, 1, 0,
                1, 3, 2, 3, 2, 0, 3, 1,
            ],
            inputs: [0, 1, 3, 4, 5, 6],
            outputs: [0, 5],
        },
        SBox {
            table: [
                0, 3, 0, 0, 2, 0, 3, 1, 1, 1, 2, 2, 2, 1, 3, 1, 2, 2, 1, 3, 2, 2, 3, 3, 0, 3, 1, 0,
                3, 2, 0, 1, 3, 0, 2, 0, 1, 0, 2, 1, 3, 3, 1, 2, 2, 0, 2, 3, 3, 2, 3, 0, 1, 1, 3, 3,
                0, 2, 1, 3, 0, 2, 2, 3,
            ],
            inputs: [0, 1, 2, 3, 5, 7],
            outputs: [1, 7],
        },
        SBox {
            table: [
                0, 1, 2, 3, 3, 3, 3, 1, 2, 0, 2, 3, 2, 1, 0, 1, 2, 2, 1, 2, 0, 3, 2, 0, 1, 1, 0, 1,
                3, 1, 3, 1, 3, 1, 0, 0, 1, 0, 0, 0, 0, 1, 2, 2, 1, 1, 3, 3, 1, 2, 3, 3, 3, 2, 3, 0,
                2, 2, 1, 3, 3, 0, 2, 0,
            ],
            inputs: [2, 3, 4, 5, 6, 7],
            outputs: [2, 3],
        },
        SBox {
            table: [
                0, 2, 1, 1, 3, 2, 0, 3, 1, 0, 1, 0, 3, 2, 1, 1, 2, 2, 0, 3, 1, 0, 1, 2, 2, 2, 3, 3,
                0, 0, 0, 0, 1, 2, 1, 0, 2, 1, 2, 2, 2, 3, 2, 3, 0, 1, 3, 0, 0, 1, 3, 0, 0, 1, 1, 0,
                1, 0, 0, 0, 0, 2, 0, 1,
            ],
            inputs: [0, 1, 2, 4, 6, 7],
            outputs: [4, 6],
        },
    ],
];

static FN2_SBOXES: [[SBox; 4]; 4] = [
    [
        // 1st round
        SBox {
            table: [
                3, 3, 0, 1, 0, 1, 0, 0, 0, 3, 0, 0, 1, 3, 1, 2, 0, 3, 3, 3, 2, 1, 0, 1, 1, 1, 2, 2,
                2, 3, 2, 2, 2, 1, 3, 3, 1, 3, 1, 1, 0, 0, 1, 2, 0, 2, 2, 1, 1, 2, 3, 1, 2, 1, 3, 1,
                2, 2, 0, 1, 3, 0, 2, 2,
            ],
            inputs: [1, 3, 4, 5, 6, 7],
            outputs: [0, 7],
        },
        SBox {
            table: [
                0, 1, 3, 0, 1, 1, 2, 3, 2, 0, 0, 3, 2, 1, 3, 1, 3, 3, 0, 0, 1, 0, 0, 3, 0, 3, 3, 2,
                3, 2, 0, 1, 3, 2, 3, 2, 2, 1, 3, 1, 1, 1, 0, 3, 3, 2, 2, 1, 1, 2, 0, 2, 0, 1, 1, 0,
                1, 0, 1, 1, 2, 0, 3, 0,
            ],
            inputs: [0, 3, 5, 6, 5, 0],
            outputs: [1, 2],
        },
        SBox {
            table: [
                0, 2, 2, 1, 0, 1, 2, 1, 2, 0, 1, 2, 3, 3, 0, 1, 3, 1, 1, 2, 1, 2, 1, 3, 3, 2, 3, 3,
                2, 1, 0, 1, 0, 1, 0, 2, 0, 1, 1, 3, 2, 0, 3, 2, 1, 1, 1, 3, 2, 3, 0, 2, 3, 0, 2, 2,
                1, 3, 0, 1, 1, 2, 2, 2,
            ],
            inputs: [0, 2, 3, 4, 7, -1],
            outputs: [3, 4],
        },
        SBox {
            table: [
                2, 3, 1, 3, 2, 0, 1, 2, 0, 0, 3, 3, 3, 3, 3, 1, 2, 0, 2, 1, 2, 3, 0, 2, 0, 1, 0, 3,
                0, 2, 1, 0, 2, 3, 0, 1, 3, 0, 3, 2, 3, 1, 2, 0, 3, 1, 1, 2, 0, 3, 0, 0, 2, 0, 2, 1,
                2, 2, 3, 2, 1, 2, 3, 1,
            ],
            inputs: [1, 2, 5, 6, -1, -1],
            outputs: [5, 6],
        },
    ],
    [
        // 2nd round
        SBox {
            table: [
                2, 3, 1, 3, 1, 0, 3, 3, 3, 2, 3, 3, 2, 0, 0, 3, 2, 3, 0, 3, 1, 1, 2, 3, 1, 1, 2, 2,
                0, 1, 0, 0, 2, 1, 0, 1, 2, 0, 1, 2, 0, 3, 1, 1, 2, 3, 1, 2, 0, 2, 0, 1, 3, 0, 1, 0,
                2, 2, 3, 0, 3, 2, 3, 0,
            ],
            inputs: [0, 1, 4, 5, 6, 7],
            outputs: [0, 7],
        },
        SBox {
            table: [
                0, 2, 2, 0, 2, 2, 0, 3, 2, 3, 2, 1, 3, 2, 3, 3, 1, 1, 0, 0, 3, 0, 2, 1, 1, 3, 3, 2,
                3, 2, 0, 1, 1, 2, 3, 0, 1, 0, 3, 0, 3, 1, 0, 2, 1, 2, 0, 3, 2, 3, 1, 2, 2, 0, 3, 2,
                3, 0, 0, 1, 2, 3, 3, 3,
            ],
            inputs: [0, 2, 3, 6, 7, -1],
            outputs: [1, 5],
        },
        SBox {
            table: [
                1, 0, 3, 0, 0, 1, 2, 1, 0, 0, 1, 0, 0, 0, 2, 3, 2, 2, 0, 2, 0, 1, 3, 0, 2, 0, 1, 3,
                2, 3, 0, 1, 1, 2, 2, 2, 1, 3, 0, 3, 0, 1, 1, 0, 3, 2, 3, 3, 2, 0, 0, 3, 1, 2, 1, 3,
                3, 2, 1, 0, 2, 1, 2, 3,
            ],
            inputs: [2, 3, 4, 6, 7, 2],
            outputs: [2, 3],
        },
        SBox {
            table: [
                2, 3, 1, 3, 1, 1, 2, 3, 3, 1, 1, 0, 1, 0, 2, 3, 2, 1, 0, 0, 2, 2, 0, 1, 0, 2, 2, 2,
                0, 2, 1, 0, 3, 1, 2, 3, 1, 3, 0, 2, 1, 0, 1, 0, 0, 1, 2, 2, 3, 2, 3, 1, 3, 2, 1, 1,
                2, 0, 2, 1, 3, 3, 1, 0,
            ],
            inputs: [1, 2, 3, 4, 5, 6],
            outputs: [4, 6],
        },
    ],
    [
        // 3rd round
        SBox {
            table: [
                0, 3, 0, 1, 3, 0, 0, 2, 1, 0, 1, 3, 2, 2, 2, 0, 3, 3, 3, 0, 2, 2, 0, 3, 0, 0, 2, 3,
                0, 3, 2, 1, 3, 3, 0, 3, 0, 2, 3, 3, 1, 1, 1, 0, 2, 2, 1, 1, 3, 0, 3, 1, 2, 0, 2, 0,
                0, 0, 3, 2, 1, 1, 0, 0,
            ],
            inputs: [1, 4, 5, 6, 7, 5],
            outputs: [0, 5],
        },
        SBox {
            table: [
                0, 3, 0, 1, 3, 0, 3, 1, 3, 2, 2, 2, 3, 0, 3, 2, 2, 1, 2, 2, 0, 3, 2, 2, 0, 0, 2, 1,
                1, 3, 2, 3, 2, 3, 3, 1, 2, 0, 1, 2, 2, 1, 0, 0, 0, 0, 2, 3, 1, 2, 0, 3, 1, 3, 1, 2,
                3, 2, 1, 0, 3, 0, 0, 2,
            ],
            inputs: [0, 2, 3, 4, 6, 7],
            outputs: [1, 7],
        },
        SBox {
            table: [
                2, 2, 0, 3, 0, 3, 1, 0, 1, 1, 2, 3, 2, 3, 1, 0, 0, 0, 3, 2, 2, 0, 2, 3, 1, 3, 2, 0,
                3, 3, 1, 3, // unused?
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            inputs: [1, 2, 4, 7, 2, -1],
            outputs: [2, 4],
        },
        SBox {
            table: [
                0, 2, 3, 1, 3, 1, 1, 0, 0, 1, 3, 0, 2, 1, 3, 3, 2, 0, 2, 1, 1, 2, 3, 3, 0, 0, 0, 2,
                0, 2, 3, 0, 3, 3, 3, 3, 2, 3, 3, 2, 3, 0, 1, 0, 2, 3, 3, 2, 0, 1, 3, 1, 0, 1, 2, 3,
                3, 0, 2, 0, 3, 0, 3, 3,
            ],
            inputs: [0, 1, 2, 3, 5, 7],
            outputs: [3, 6],
        },
    ],
    [
        // 4th round
        SBox {
            table: [
                0, 1, 1, 0, 0, 1, 0, 2, 3, 3, 0, 1, 2, 3, 0, 2, 1, 0, 3, 3, 2, 0, 3, 0, 0, 2, 1, 0,
                1, 0, 1, 3, 0, 3, 3, 1, 2, 0, 3, 0, 1, 3, 2, 0, 3, 3, 1, 3, 0, 2, 3, 3, 2, 1, 1, 2,
                2, 1, 2, 1, 2, 0, 1, 1,
            ],
            inputs: [0, 1, 2, 4, 7, -1],
            outputs: [0, 5],
        },
        SBox {
            table: [
                2, 0, 0, 2, 3, 0, 2, 3, 3, 1, 1, 1, 2, 1, 1, 0, 0, 2, 1, 0, 0, 3, 1, 0, 0, 3, 3, 0,
                1, 0, 1, 2, 0, 2, 0, 2, 0, 1, 2, 3, 2, 1, 1, 0, 3, 3, 3, 3, 3, 3, 1, 0, 3, 0, 0, 2,
                0, 3, 2, 0, 2, 2, 0, 1,
            ],
            inputs: [0, 1, 3, 5, 6, -1],
            outputs: [1, 3],
        },
        SBox {
            table: [
                0, 1, 1, 2, 1, 3, 1, 1, 0, 0, 3, 1, 1, 1, 2, 0, 3, 2, 0, 1, 1, 2, 3, 3, 3, 0, 3, 0,
                0, 2, 0, 3, 3, 2, 0, 0, 3, 2, 3, 1, 2, 3, 0, 3, 2, 0, 1, 2, 2, 2, 0, 2, 0, 1, 2, 2,
                3, 1, 2, 2, 1, 1, 1, 1,
            ],
            inputs: [0, 2, 3, 4, 5, 7],
            outputs: [2, 7],
        },
        SBox {
            table: [
                0, 1, 2, 0, 3, 3, 0, 3, 2, 1, 3, 3, 0, 3, 1, 1, 3, 2, 3, 2, 3, 0, 0, 0, 3, 0, 2, 2,
                3, 2, 2, 3, 2, 2, 3, 1, 2, 3, 1, 2, 0, 3, 0, 2, 3, 1, 0, 0, 3, 2, 1, 2, 1, 2, 1, 3,
                1, 0, 2, 3, 3, 1, 3, 2,
            ],
            inputs: [2, 3, 4, 5, 6, 7],
            outputs: [4, 6],
        },
    ],
];
