//! Yamaha YMW-258-F "MultiPCM" (Sega 315-5560) /
//! `multipcm.cpp` (Miguel Angel Horna's implementation, BSD-3-Clause).
//!
//! 28 PCM voices, each with a 4-stage envelope, pitch/amplitude LFOs, a pan
//! matrix, and linear interpolation between source samples. Sample metadata
//! (start/loop/end/envelope/LFO) lives in the first part of the sample ROM as a
//! 12-byte-per-instrument table.
//!
//! The port is deliberately literal -- same tables, same fixed-point shifts,
//! same update order -- so that its output can be compared against the reference if a
//! sound bug ever needs pinning down.

// The register and table loops index the same way the hardware documents
// them, which is what makes them checkable against the reference.
#![allow(clippy::needless_range_loop)]

const TL_SHIFT: u32 = 12;
const EG_SHIFT: u32 = 16;
const LFO_SHIFT: u32 = 8;

const VOICES: usize = 28;
const CLOCK_DIVIDER: f32 = 224.0;

/// Envelope times in milliseconds on a 44100Hz timebase.
const BASE_TIMES: [f64; 64] = [
    0.0, 0.0, 0.0, 0.0, 6222.95, 4978.37, 4148.66, 3556.01, 3111.47, 2489.21, 2074.33, 1778.00,
    1555.74, 1244.63, 1037.19, 889.02, 777.87, 622.31, 518.59, 444.54, 388.93, 311.16, 259.32,
    222.27, 194.47, 155.60, 129.66, 111.16, 97.23, 77.82, 64.85, 55.60, 48.62, 38.91, 32.43, 27.80,
    24.31, 19.46, 16.24, 13.92, 12.15, 9.75, 8.12, 6.98, 6.08, 4.90, 4.08, 3.49, 3.04, 2.49, 2.13,
    1.90, 1.72, 1.41, 1.18, 1.04, 0.91, 0.73, 0.59, 0.50, 0.45, 0.45, 0.45, 0.45,
];

const LFO_FREQ: [f32; 8] = [0.168, 2.019, 3.196, 4.206, 5.215, 5.888, 6.224, 7.066];
const PHASE_SCALE_LIMIT: [f32; 8] = [0.0, 3.378, 5.065, 6.750, 10.114, 20.170, 40.180, 79.307];
const AMPLITUDE_SCALE_LIMIT: [f32; 8] = [0.0, 0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0];

/// Register slot-select values to voice numbers.
const VALUE_TO_CHANNEL: [i32; 32] = [
    0, 1, 2, 3, 4, 5, 6, -1, 7, 8, 9, 10, 11, 12, 13, -1, 14, 15, 16, 17, 18, 19, 20, -1, 21, 22,
    23, 24, 25, 26, 27, -1,
];

fn value_to_fixed(bits: u32, value: f32) -> u32 {
    ((1u64 << bits) as f32 * value) as u32
}

#[derive(Clone, Copy, Default)]
struct Sample {
    start: u32,
    loop_pt: u32,
    end: u32,
    attack_reg: u8,
    decay1_reg: u8,
    decay2_reg: u8,
    decay_level: u8,
    release_reg: u8,
    key_rate_scale: u8,
    lfo_vibrato_reg: u8,
    lfo_amplitude_reg: u8,
    format: u8,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum EgState {
    #[default]
    Attack,
    Decay1,
    Decay2,
    Release,
}

#[derive(Clone, Copy)]
struct Envelope {
    volume: i32,
    state: EgState,
    attack_rate: i32,
    decay1_rate: i32,
    decay2_rate: i32,
    release_rate: i32,
    decay_level: i32,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            volume: 0,
            state: EgState::Attack,
            attack_rate: 0,
            decay1_rate: 0,
            decay2_rate: 0,
            release_rate: 0,
            decay_level: 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Lfo {
    phase: u16,
    phase_step: u32,
    /// Index into the shared tables: false = pitch, true = amplitude.
    amplitude: bool,
    scale_sel: usize,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    regs: [u8; 8],
    playing: bool,
    sample: Sample,
    offset: u32,
    octave: u8,
    pitch: u16,
    step: u32,
    reverse: bool,
    pan: u32,
    total_level: u32,
    dest_total_level: u32,
    total_level_step: i32,
    prev_sample: i32,
    envelope: Envelope,
    lfo_frequency: u8,
    pitch_lfo: Lfo,
    vibrato: u8,
    amplitude_lfo: Lfo,
    tremolo: u8,
}

pub struct MultiPcm {
    rom: Vec<u8>,
    /// 1MB bank mapped at chip addresses 0x100000-0x1fffff (`m1_snd_mpcm_bnk_w`).
    bank: usize,
    slots: [Slot; VOICES],
    cur_slot: usize,
    address: usize,
    rate: f32,

    attack_step: [u32; 0x40],
    decay_release_step: [u32; 0x40],
    freq_step_table: [u32; 0x400],
    left_pan_table: [i32; 0x800],
    right_pan_table: [i32; 0x800],
    linear_to_exp_volume: [i32; 0x400],
    total_level_steps: [i32; 2],
    pitch_table: [i32; 256],
    amplitude_table: [i32; 256],
    pitch_scale_tables: [[i32; 256]; 8],
    amplitude_scale_tables: [[i32; 256]; 8],
    pub writes: u64,
    pub key_ons: u64,
}

impl MultiPcm {
    /// Lightweight diagnostics used by the sound-board probes.
    pub fn active_voices(&self) -> usize {
        self.slots.iter().filter(|slot| slot.playing).count()
    }

    pub fn active_samples(&self) -> Vec<(usize, u16, u32)> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.playing)
            .map(|(voice, slot)| {
                let sample = slot.regs[1] as u16 | (((slot.regs[2] & 1) as u16) << 8);
                (voice, sample, slot.sample.start)
            })
            .collect()
    }

    pub fn new(rom: Vec<u8>, clock_hz: f32) -> Self {
        let rate = clock_hz / CLOCK_DIVIDER;
        let mut m = Self {
            rom,
            bank: 0,
            slots: [Slot::default(); VOICES],
            cur_slot: 0,
            address: 0,
            rate,
            attack_step: [0; 0x40],
            decay_release_step: [0; 0x40],
            freq_step_table: [0; 0x400],
            left_pan_table: [0; 0x800],
            right_pan_table: [0; 0x800],
            linear_to_exp_volume: [0; 0x400],
            total_level_steps: [0; 2],
            pitch_table: [0; 256],
            amplitude_table: [0; 256],
            pitch_scale_tables: [[0; 256]; 8],
            amplitude_scale_tables: [[0; 256]; 8],
            writes: 0,
            key_ons: 0,
        };
        m.build_tables();
        m
    }

    pub fn sample_rate(&self) -> f32 {
        self.rate
    }

    pub fn set_bank(&mut self, entry: u32) {
        self.bank = (entry & 3) as usize;
    }

    fn build_tables(&mut self) {
        // Volume + pan matrix.
        for level in 0..0x80usize {
            let vol_db = level as f32 * -24.0 / 64.0;
            let total = 10f32.powf(vol_db / 20.0) / 4.0;
            for pan in 0..0x10usize {
                let (pan_left, pan_right) = if pan == 0x8 {
                    (0.0, 0.0)
                } else if pan == 0x0 {
                    (1.0, 1.0)
                } else if pan & 0x8 != 0 {
                    let inverted = 0x10 - pan;
                    let db = inverted as f32 * -12.0 / 4.0;
                    let r = if inverted & 7 == 7 {
                        0.0
                    } else {
                        10f32.powf(db / 20.0)
                    };
                    (1.0, r)
                } else {
                    let db = pan as f32 * -12.0 / 4.0;
                    let l = if pan & 7 == 7 {
                        0.0
                    } else {
                        10f32.powf(db / 20.0)
                    };
                    (l, 1.0)
                };
                self.left_pan_table[(pan << 7) | level] =
                    value_to_fixed(TL_SHIFT, pan_left * total) as i32;
                self.right_pan_table[(pan << 7) | level] =
                    value_to_fixed(TL_SHIFT, pan_right * total) as i32;
            }
        }

        // Pitch steps.
        for i in 0..0x400usize {
            let fcent = self.rate * (1024.0 + i as f32) / 1024.0;
            self.freq_step_table[i] = value_to_fixed(TL_SHIFT, fcent);
        }

        // Envelope steps.
        for i in 4..0x40usize {
            self.attack_step[i] =
                ((0x400u64 << EG_SHIFT) as f64 / (BASE_TIMES[i] * 44100.0 / 1000.0)) as u32;
            self.decay_release_step[i] = ((0x400u64 << EG_SHIFT) as f64
                / (BASE_TIMES[i] * 14.32833 * 44100.0 / 1000.0))
                as u32;
        }
        self.attack_step[0x3f] = 0x400 << EG_SHIFT;

        // Total-level interpolation.
        self.total_level_steps[0] =
            -(((0x80 << TL_SHIFT) as f32) / (78.2 * 44100.0 / 1000.0)) as i32;
        self.total_level_steps[1] =
            (((0x80 << TL_SHIFT) as f32) / (78.2 * 2.0 * 44100.0 / 1000.0)) as i32;

        // Linear -> exponential volume.
        for i in 0..0x400usize {
            let db = -(96.0 - 96.0 * i as f32 / 0x400 as f32);
            self.linear_to_exp_volume[i] = value_to_fixed(TL_SHIFT, 10f32.powf(db / 20.0)) as i32;
        }

        // LFO tables.
        for i in 0..256usize {
            self.pitch_table[i] = if i < 64 {
                (i as i32) * 2 + 128
            } else if i < 128 {
                383 - (i as i32) * 2
            } else if i < 192 {
                384 - (i as i32) * 2
            } else {
                (i as i32) * 2 - 383
            };
            self.amplitude_table[i] = if i < 128 {
                255 - (i as i32) * 2
            } else {
                (i as i32) * 2 - 256
            };
        }
        for table in 0..8usize {
            let limit = PHASE_SCALE_LIMIT[table];
            for i in -128i32..128 {
                let value = limit * i as f32 / 128.0;
                let converted = 2f32.powf(value / 1200.0);
                self.pitch_scale_tables[table][(i + 128) as usize] =
                    value_to_fixed(LFO_SHIFT, converted) as i32;
            }
            let limit = -AMPLITUDE_SCALE_LIMIT[table];
            for i in 0..256usize {
                let value = limit * i as f32 / 256.0;
                let converted = 10f32.powf(value / 20.0);
                self.amplitude_scale_tables[table][i] = value_to_fixed(LFO_SHIFT, converted) as i32;
            }
        }
    }

    /// Chip address space: first 1MB fixed, second 1MB banked (`mpcm1_map`).
    fn read_byte(&self, addr: u32) -> u8 {
        let a = addr as usize & 0x3f_ffff;
        let idx = match a {
            0x00_0000..=0x0f_ffff => a,
            0x10_0000..=0x1f_ffff => self.bank * 0x10_0000 + (a & 0xf_ffff),
            _ => return 0,
        };
        self.rom.get(idx).copied().unwrap_or(0)
    }

    fn init_sample(&self, index: u32) -> Sample {
        let a = index * 12;
        let rb = |o: u32| self.read_byte(a + o) as u32;
        let mut start = (rb(0) << 16) | (rb(1) << 8) | rb(2);
        let format = ((start >> 20) & 0xfe) as u8;
        start &= 0x3f_ffff;
        Sample {
            start,
            loop_pt: (rb(3) << 8) | rb(4),
            end: 0x10000 - ((rb(5) << 8) | rb(6)),
            attack_reg: ((rb(8) >> 4) & 0xf) as u8,
            decay1_reg: (rb(8) & 0xf) as u8,
            decay2_reg: (rb(9) & 0xf) as u8,
            decay_level: ((rb(9) >> 4) & 0xf) as u8,
            release_reg: (rb(10) & 0xf) as u8,
            key_rate_scale: ((rb(10) >> 4) & 0xf) as u8,
            lfo_vibrato_reg: rb(7) as u8,
            lfo_amplitude_reg: (rb(11) & 0xf) as u8,
            format,
        }
    }

    fn get_rate(steps: &[u32; 0x40], rate: i32, val: u32) -> u32 {
        if val == 0 {
            return steps[0];
        }
        if val == 0xf {
            return steps[0x3f];
        }
        steps[(4 * val as i32 + rate).clamp(0, 0x3f) as usize]
    }

    fn envelope_calc(&self, slot: &mut Slot) {
        let mut octave = slot.octave as i32;
        if octave & 8 != 0 {
            octave -= 16;
        }
        let rate = if slot.sample.key_rate_scale != 0xf {
            (octave + slot.sample.key_rate_scale as i32) * 2 + ((slot.pitch >> 9) & 1) as i32
        } else {
            0
        };
        let e = &mut slot.envelope;
        e.attack_rate =
            Self::get_rate(&self.attack_step, rate, slot.sample.attack_reg as u32) as i32;
        e.decay1_rate = Self::get_rate(
            &self.decay_release_step,
            rate,
            slot.sample.decay1_reg as u32,
        ) as i32;
        e.decay2_rate = Self::get_rate(
            &self.decay_release_step,
            rate,
            slot.sample.decay2_reg as u32,
        ) as i32;
        e.release_rate = Self::get_rate(
            &self.decay_release_step,
            rate,
            slot.sample.release_reg as u32,
        ) as i32;
        e.decay_level = 0xf - slot.sample.decay_level as i32;
    }

    fn retrigger(&self, slot: &mut Slot) {
        slot.offset = 0;
        slot.prev_sample = 0;
        slot.total_level = slot.dest_total_level << TL_SHIFT;
        self.envelope_calc(slot);
        slot.envelope.state = EgState::Attack;
        slot.envelope.volume = 0;
    }

    fn update_step(&self, slot: &mut Slot) {
        let oct = (slot.octave.wrapping_sub(1)) & 0xf;
        let mut pitch = self.freq_step_table[slot.pitch as usize];
        if oct & 8 != 0 {
            pitch >>= 16 - oct as u32;
        } else {
            pitch <<= oct as u32;
        }
        slot.step = (pitch as f32 / self.rate) as u32;
    }

    fn lfo_compute_step(&self, lfo: &mut Lfo, frequency: u8, scale: u8, amplitude: bool) {
        let step = LFO_FREQ[frequency as usize] * 256.0 / self.rate;
        lfo.phase_step = ((1u32 << LFO_SHIFT) as f32 * step) as u32;
        lfo.amplitude = amplitude;
        lfo.scale_sel = (scale & 7) as usize;
    }

    fn lfo_step(&self, lfo: &mut Lfo) -> i32 {
        lfo.phase = lfo.phase.wrapping_add(lfo.phase_step as u16);
        let idx = ((lfo.phase as u32) >> LFO_SHIFT) as usize & 0xff;
        let p = if lfo.amplitude {
            self.amplitude_scale_tables[lfo.scale_sel][self.amplitude_table[idx] as usize & 0xff]
        } else {
            self.pitch_scale_tables[lfo.scale_sel][self.pitch_table[idx] as usize & 0xff]
        };
        p << (TL_SHIFT - LFO_SHIFT)
    }

    fn envelope_update(&self, slot: &mut Slot) -> i32 {
        let e = &mut slot.envelope;
        match e.state {
            EgState::Attack => {
                e.volume += e.attack_rate;
                if e.volume >= (0x3ff << EG_SHIFT) {
                    e.state = EgState::Decay1;
                    if e.decay1_rate >= (0x400 << EG_SHIFT) {
                        e.state = EgState::Decay2;
                    }
                    e.volume = 0x3ff << EG_SHIFT;
                }
            }
            EgState::Decay1 => {
                e.volume -= e.decay1_rate;
                if e.volume <= 0 {
                    e.volume = 0;
                }
                if e.volume >> (EG_SHIFT + 6) <= e.decay_level {
                    e.state = EgState::Decay2;
                }
            }
            EgState::Decay2 => {
                e.volume -= e.decay2_rate;
                if e.volume <= 0 {
                    e.volume = 0;
                }
            }
            EgState::Release => {
                e.volume -= e.release_rate;
                if e.volume <= 0 {
                    e.volume = 0;
                    slot.playing = false;
                }
            }
        }
        self.linear_to_exp_volume[(slot.envelope.volume >> EG_SHIFT) as usize & 0x3ff]
    }

    fn write_slot(&mut self, slot_idx: usize, reg: usize, data: u8) {
        // `self.slots[slot_idx]` is Copy; work on a local and put it back, so
        // the table lookups on `self` stay borrowable.
        let mut slot = self.slots[slot_idx];
        slot.regs[reg] = data;
        match reg {
            0 => slot.pan = ((data >> 4) & 0xf) as u32,
            1 => {
                slot.sample =
                    self.init_sample(slot.regs[1] as u32 | (((slot.regs[2] & 1) as u32) << 8));
                // A sample write loads the LFO registers from the metadata.
                let vib = slot.sample.lfo_vibrato_reg;
                let amp = slot.sample.lfo_amplitude_reg;
                self.slots[slot_idx] = slot;
                self.write_slot(slot_idx, 6, vib);
                self.write_slot(slot_idx, 7, amp);
                slot = self.slots[slot_idx];
                if slot.playing {
                    self.retrigger(&mut slot);
                }
            }
            2 | 3 => {
                slot.octave = slot.regs[3] >> 4;
                slot.pitch = (((slot.regs[3] & 0xf) as u16) << 6) | ((slot.regs[2] >> 2) as u16);
                self.update_step(&mut slot);
            }
            4 => {
                if data & 0x80 != 0 {
                    slot.playing = true;
                    self.retrigger(&mut slot);
                } else if slot.playing {
                    if slot.sample.release_reg != 0xf {
                        slot.envelope.state = EgState::Release;
                    } else {
                        slot.playing = false;
                    }
                }
            }
            5 => {
                slot.dest_total_level = ((data >> 1) & 0x7f) as u32;
                if data & 1 == 0 {
                    slot.total_level_step = if slot.total_level >> TL_SHIFT > slot.dest_total_level
                    {
                        self.total_level_steps[0]
                    } else {
                        self.total_level_steps[1]
                    };
                } else {
                    slot.total_level = slot.dest_total_level << TL_SHIFT;
                }
            }
            6 | 7 => {
                slot.lfo_frequency = (slot.regs[6] >> 3) & 7;
                slot.vibrato = slot.regs[6] & 7;
                slot.tremolo = slot.regs[7] & 7;
                if data != 0 {
                    let (f, v, t) = (slot.lfo_frequency, slot.vibrato, slot.tremolo);
                    self.lfo_compute_step(&mut slot.pitch_lfo, f, v, false);
                    self.lfo_compute_step(&mut slot.amplitude_lfo, f, t, true);
                }
            }
            _ => {}
        }
        self.slots[slot_idx] = slot;
    }

    /// Register interface: offset 0 = data,
    /// 1 = slot select, 2 = address select.
    pub fn write(&mut self, offset: u32, data: u8) {
        self.writes += 1;
        match offset & 3 {
            0 => {
                if self.address == 4 && data & 0x80 != 0 {
                    self.key_ons += 1;
                }
                self.write_slot(self.cur_slot, self.address, data)
            }
            1 => {
                let ch = VALUE_TO_CHANNEL[(data & 0x1f) as usize];
                if ch >= 0 {
                    self.cur_slot = ch as usize;
                }
            }
            2 => self.address = (data as usize).min(7),
            _ => {}
        }
    }

    pub fn read(&self) -> u8 {
        0
    }

    /// Renders one stereo sample pair at the chip's native rate.
    pub fn generate(&mut self) -> (i32, i32) {
        let mut smpl = 0i32;
        let mut smpr = 0i32;
        for sl in 0..VOICES {
            let mut slot = self.slots[sl];
            if !slot.playing {
                continue;
            }
            let vol = ((slot.total_level >> TL_SHIFT) | (slot.pan << 7)) as usize & 0x7ff;
            let mut spos = slot.offset >> TL_SHIFT;
            let mut step = slot.step;
            let fpart = (slot.offset & ((1 << TL_SHIFT) - 1)) as i32;

            if slot.reverse {
                spos = slot.sample.end - spos - 1;
            }

            let csample: i32 = if slot.sample.format & 4 != 0 {
                // 12-bit linear, packed 2 samples / 3 bytes.
                let adr = slot.sample.start + (spos >> 1) * 3;
                if spos & 1 == 0 {
                    (((self.read_byte(adr) as i32) << 8)
                        | (((self.read_byte(adr + 1) & 0xf) as i32) << 4))
                        as i16 as i32
                } else {
                    (((self.read_byte(adr + 2) as i32) << 8)
                        | ((self.read_byte(adr + 1) & 0xf0) as i32)) as i16
                        as i32
                }
            } else {
                ((self.read_byte(slot.sample.start + spos) as i8) as i32) << 8
            };

            let mut sample =
                (csample * fpart + slot.prev_sample * ((1 << TL_SHIFT) - fpart)) >> TL_SHIFT;

            if slot.vibrato != 0 {
                step =
                    ((step as u64 * self.lfo_step(&mut slot.pitch_lfo) as u64) >> TL_SHIFT) as u32;
            }
            slot.offset += step;
            if spos ^ (slot.offset >> TL_SHIFT) != 0 {
                slot.prev_sample = csample;
            }
            if slot.offset >= (slot.sample.end << TL_SHIFT) {
                slot.offset -= (slot.sample.end - slot.sample.loop_pt) << TL_SHIFT;
                slot.reverse = false;
            }
            if slot.total_level >> TL_SHIFT != slot.dest_total_level {
                slot.total_level = (slot.total_level as i32 + slot.total_level_step) as u32;
            }
            if slot.tremolo != 0 {
                sample = (sample * self.lfo_step(&mut slot.amplitude_lfo)) >> TL_SHIFT;
            }
            sample = (sample * self.envelope_update(&mut slot)) >> 10;

            smpl += (self.left_pan_table[vol] * sample) >> TL_SHIFT;
            smpr += (self.right_pan_table[vol] * sample) >> TL_SHIFT;

            self.slots[sl] = slot;
        }
        (smpl, smpr)
    }
}
