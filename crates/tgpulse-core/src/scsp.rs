//! Sega/Yamaha YMF292-F, the Saturn Custom Sound Processor, as fitted to the
//! Model 2A, 2B and 2C sound boards.
//!
//! The chip has 32 voices. Each can play a sample or take part in an FM
//! construct -- and unlike a traditional Yamaha FM part, the base waveform for
//! that construct still comes from the wavetable RAM. It also carries an
//! on-board DSP for effects, three timers, a DMA engine and a MIDI pair.
//!
//! The implementation is deliberately literal: the same tables, the same
//! fixed-point shifts, the same update order, so that a sound bug can be
//! pinned down by comparing output rather than by reasoning about it. On
//! Model 2A the chip's own address space maps only the shared 512KB sound RAM
//! (`ram`); its register window is the 68000's 0x100000-0x100fff range.

// The register and table loops index the same way the hardware documents
// them, which is what makes them checkable against the reference.
#![allow(clippy::needless_range_loop)]

/// SCSP master clock (Model 2A: 45.1584 MHz XTAL / 2).
const CLOCK: u32 = 22_579_200;

const SHIFT: u32 = 12;
const LFO_SHIFT: u32 = 8;
const EG_SHIFT: u32 = 16;

// Interrupt source numbers within SCIPD/SCIEB/SCILVx.
const SCIMID: u32 = 3;
const SCIDMA: u32 = 4;
const SCITMA: u32 = 6;
const SCITMB: u32 = 7;

// Envelope times in ms
const AR_TIMES: [f64; 64] = [
    100000.0, /*infinity*/
    100000.0, /*infinity*/
    8100.0, 6900.0, 6000.0, 4800.0, 4000.0, 3400.0, 3000.0, 2400.0, 2000.0, 1700.0, 1500.0, 1200.0,
    1000.0, 860.0, 760.0, 600.0, 500.0, 430.0, 380.0, 300.0, 250.0, 220.0, 190.0, 150.0, 130.0,
    110.0, 95.0, 76.0, 63.0, 55.0, 47.0, 38.0, 31.0, 27.0, 24.0, 19.0, 15.0, 13.0, 12.0, 9.4, 7.9,
    6.8, 6.0, 4.7, 3.8, 3.4, 3.0, 2.4, 2.0, 1.8, 1.6, 1.3, 1.1, 0.93, 0.85, 0.65, 0.53, 0.44, 0.40,
    0.35, 0.0, 0.0,
];
const DR_TIMES: [f64; 64] = [
    100000.0, /*infinity*/
    100000.0, /*infinity*/
    118200.0, 101300.0, 88600.0, 70900.0, 59100.0, 50700.0, 44300.0, 35500.0, 29600.0, 25300.0,
    22200.0, 17700.0, 14800.0, 12700.0, 11100.0, 8900.0, 7400.0, 6300.0, 5500.0, 4400.0, 3700.0,
    3200.0, 2800.0, 2200.0, 1800.0, 1600.0, 1400.0, 1100.0, 920.0, 790.0, 690.0, 550.0, 460.0,
    390.0, 340.0, 270.0, 230.0, 200.0, 170.0, 140.0, 110.0, 98.0, 85.0, 68.0, 57.0, 49.0, 43.0,
    34.0, 28.0, 25.0, 22.0, 18.0, 14.0, 12.0, 11.0, 8.5, 7.1, 6.1, 5.4, 4.3, 3.6, 3.1,
];

const SDLT: [f32; 8] = [-1000000.0, -36.0, -30.0, -24.0, -18.0, -12.0, -6.0, 0.0];

// LFO handling
const LFO_FREQ: [f32; 32] = [
    0.17, 0.19, 0.23, 0.27, 0.34, 0.39, 0.45, 0.55, 0.68, 0.78, 0.92, 1.10, 1.39, 1.60, 1.87, 2.27,
    2.87, 3.31, 3.92, 4.79, 6.15, 7.18, 8.60, 10.8, 14.4, 17.2, 21.5, 28.7, 43.1, 57.4, 86.1,
    172.3,
];
const ASCALE: [f32; 8] = [0.0, 0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0];
const PSCALE: [f32; 8] = [0.0, 7.0, 13.5, 27.0, 55.0, 112.0, 230.0, 494.0];

/// DSP PACK: 24-bit linear -> 16-bit floating point (sign/exponent/mantissa)
/// used by the DSP's delay-memory write path when NOFL=0.
fn dsp_pack(val: i32) -> u16 {
    let sign = (val >> 23) & 1;
    let mut temp = ((val ^ val.wrapping_shl(1)) & 0xFFFFFF) as u32;
    let mut exponent = 0;
    for _ in 0..12 {
        if temp & 0x800000 != 0 {
            break;
        }
        temp <<= 1;
        exponent += 1;
    }
    let mut v = val;
    if exponent < 12 {
        v = (val << exponent) & 0x3FFFFF;
    } else {
        v <<= 11;
    }
    v >>= 11;
    v &= 0x7FF;
    v |= sign << 15;
    v |= exponent << 11;
    v as u16
}

/// DSP UNPACK: 16-bit floating point -> 24-bit linear (delay-memory read path).
fn dsp_unpack(val: u16) -> i32 {
    let sign = ((val >> 15) & 1) as i32;
    let mut exponent = ((val >> 11) & 0xF) as i32;
    let mantissa = (val & 0x7FF) as i32;
    let mut uval = mantissa << 11;
    if exponent > 11 {
        exponent = 11;
        uval |= sign << 22;
    } else {
        uval |= (sign ^ 1) << 22;
    }
    uval |= sign << 23;
    uval <<= 8;
    uval >>= 8;
    uval >>= exponent;
    uval
}

#[inline]
fn sext24(v: i32) -> i32 {
    (v << 8) >> 8
}

#[inline]
fn sext13(v: i32) -> i32 {
    (v << 19) >> 19
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum EgState {
    Attack = 0,
    Decay1 = 1,
    Decay2 = 2,
    #[default]
    Release = 3,
}

#[derive(Clone, Copy, Default)]
struct Eg {
    volume: i32,
    state: EgState,
    // step vals
    ar: i32,  // Attack
    d1r: i32, // Decay1
    d2r: i32, // Decay2
    rr: i32,  // Release
    dl: i32,  // Decay level
    eghold: bool,
}

#[derive(Clone, Copy, Default)]
struct Lfo {
    phase: u16,
    phase_step: u32,
    wave: u8,  // 0=saw 1=square 2=triangle 3=noise
    scale: u8, // index into PSCALES/ASCALES
}

#[derive(Clone, Copy, Default)]
struct Slot {
    data: [u16; 0x10], // raw slot registers
    backwards: bool,   // the wave is playing backwards
    active: bool,      // this slot is currently playing
    cur_addr: u32,     // current play address (24.8)
    nxt_addr: u32,     // next play address
    step: u32,         // pitch step (24.8)
    eg: Eg,
    plfo: Lfo,
    alfo: Lfo,
}

// SLOT PARAMETERS
impl Slot {
    #[inline]
    fn keyonex(&self) -> bool {
        self.data[0x0] & 0x1000 != 0
    }
    #[inline]
    fn keyonb(&self) -> bool {
        self.data[0x0] & 0x0800 != 0
    }
    #[inline]
    fn sbctl(&self) -> u32 {
        ((self.data[0x0] >> 0x9) & 0x0003) as u32
    }
    #[inline]
    fn ssctl(&self) -> u32 {
        ((self.data[0x0] >> 0x7) & 0x0003) as u32
    }
    #[inline]
    fn lpctl(&self) -> u32 {
        ((self.data[0x0] >> 0x5) & 0x0003) as u32
    }
    #[inline]
    fn pcm8b(&self) -> bool {
        self.data[0x0] & 0x0010 != 0
    }
    #[inline]
    fn sa(&self) -> u32 {
        (((self.data[0x0] & 0xF) as u32) << 16) | self.data[0x1] as u32
    }
    #[inline]
    fn lsa(&self) -> u32 {
        self.data[0x2] as u32
    }
    #[inline]
    fn lea(&self) -> u32 {
        self.data[0x3] as u32
    }
    #[inline]
    fn d2r(&self) -> u32 {
        ((self.data[0x4] >> 0xB) & 0x001F) as u32
    }
    #[inline]
    fn d1r(&self) -> u32 {
        ((self.data[0x4] >> 0x6) & 0x001F) as u32
    }
    #[inline]
    fn eghold(&self) -> bool {
        self.data[0x4] & 0x0020 != 0
    }
    #[inline]
    fn ar(&self) -> u32 {
        (self.data[0x4] & 0x001F) as u32
    }
    #[inline]
    fn lpslnk(&self) -> bool {
        self.data[0x5] & 0x4000 != 0
    }
    #[inline]
    fn krs(&self) -> u32 {
        ((self.data[0x5] >> 0xA) & 0x000F) as u32
    }
    #[inline]
    fn dl(&self) -> u32 {
        ((self.data[0x5] >> 0x5) & 0x001F) as u32
    }
    #[inline]
    fn rr(&self) -> u32 {
        (self.data[0x5] & 0x001F) as u32
    }
    #[inline]
    fn stwinh(&self) -> bool {
        self.data[0x6] & 0x0200 != 0
    }
    #[inline]
    fn sdir(&self) -> bool {
        self.data[0x6] & 0x0100 != 0
    }
    #[inline]
    fn tl(&self) -> u32 {
        (self.data[0x6] & 0x00FF) as u32
    }
    #[inline]
    fn mdl(&self) -> u32 {
        ((self.data[0x7] >> 0xC) & 0x000F) as u32
    }
    #[inline]
    fn mdxsl(&self) -> u32 {
        ((self.data[0x7] >> 0x6) & 0x003F) as u32
    }
    #[inline]
    fn mdysl(&self) -> u32 {
        (self.data[0x7] & 0x003F) as u32
    }
    #[inline]
    fn oct(&self) -> u32 {
        ((self.data[0x8] >> 0xB) & 0x000F) as u32
    }
    #[inline]
    fn fns(&self) -> u32 {
        (self.data[0x8] & 0x03FF) as u32
    }
    #[inline]
    fn lfof(&self) -> u32 {
        ((self.data[0x9] >> 0xA) & 0x001F) as u32
    }
    #[inline]
    fn plfows(&self) -> u32 {
        ((self.data[0x9] >> 0x8) & 0x0003) as u32
    }
    #[inline]
    fn plfos(&self) -> u32 {
        ((self.data[0x9] >> 0x5) & 0x0007) as u32
    }
    #[inline]
    fn alfows(&self) -> u32 {
        ((self.data[0x9] >> 0x3) & 0x0003) as u32
    }
    #[inline]
    fn alfos(&self) -> u32 {
        (self.data[0x9] & 0x0007) as u32
    }
    #[inline]
    fn isel(&self) -> u32 {
        ((self.data[0xA] >> 0x3) & 0x000F) as u32
    }
    #[inline]
    fn imxl(&self) -> u32 {
        (self.data[0xA] & 0x0007) as u32
    }
    #[inline]
    fn disdl(&self) -> u32 {
        ((self.data[0xB] >> 0xD) & 0x0007) as u32
    }
    #[inline]
    fn dipan(&self) -> u32 {
        ((self.data[0xB] >> 0x8) & 0x001F) as u32
    }
    #[inline]
    fn efsdl(&self) -> u32 {
        ((self.data[0xB] >> 0x5) & 0x0007) as u32
    }
    #[inline]
    fn efpan(&self) -> u32 {
        (self.data[0xB] & 0x001F) as u32
    }
}

/// DMA state (DMEA/DRGA/DTLG/DDIR/DGATE).
#[derive(Clone, Copy, Default)]
struct Dma {
    dmea: u32,
    drga: u16,
    dtlg: u16,
    dgate: bool,
    ddir: bool,
}

/// The DSP context.
#[derive(Clone, Copy)]
struct Dsp {
    // Config
    rbp: u32, // Ring buf pointer
    rbl: u32, // Delay ram (Ring buffer) size in words
    // context
    coef: [i16; 64],      // 16 bit signed
    madrs: [u16; 32],     // offsets (in words), 16 bit
    mpro: [u16; 128 * 4], // 128 steps 64 bit
    temp: [i32; 128],     // TEMP regs, 24 bit signed
    mems: [i32; 32],      // MEMS regs, 24 bit signed
    dec: u32,
    // input
    mixs: [i32; 16], // MIXS, 24 bit signed
    exts: [i16; 2],  // External inputs (CDDA on Saturn; nothing on Model 2A), 16 bit signed
    // output
    efreg: [i16; 16], // EFREG, 16 bit signed
    stopped: bool,
    last_step: usize,
}

impl Default for Dsp {
    fn default() -> Self {
        // SCSPDSP::Init(): everything zero except...
        Dsp {
            rbp: 0,
            rbl: 8 * 1024, // Initial RBL is 0
            coef: [0; 64],
            madrs: [0; 32],
            mpro: [0; 128 * 4],
            temp: [0; 128],
            mems: [0; 32],
            dec: 0,
            mixs: [0; 16],
            exts: [0; 2],
            efreg: [0; 16],
            stopped: true,
            last_step: 0,
        }
    }
}

pub struct Scsp {
    /// 512KB work RAM shared with the 68000. The SCSP's
    /// whole sample/DSP address space on Model 2A (accesses mask to 0x7ffff).
    pub ram: Vec<u8>,

    udata: [u16; 0x30 / 2], // common registers
    slots: [Slot; 32],
    ringbuf: [i16; 128],
    bufptr: u8,

    latched_mslc: u8,
    latched_mslc_data: u16,

    // Decoded IRQ levels (DecodeSCI results), and the current state of the
    // single interrupt output.
    irq_tim_a: u32,
    irq_tim_bc: u32,
    irq_midi: u32,
    irq_value: u32,
    irq_asserted: bool,

    midi_out_stack: [u8; 32],
    midi_out_w: u8,
    midi_out_r: u8,
    midi_out_queue: std::collections::VecDeque<u8>,
    pub midi_in_count: u64,
    pub midi_out_count: u64,
    midi_stack: [u8; 32],
    midi_w: u8,
    midi_r: u8,

    eg_table: [i32; 0x400],
    lpantable: Box<[i32; 0x10000]>,
    rpantable: Box<[i32; 0x10000]>,

    timpris: [u32; 3],
    timcnt: [i32; 3],
    // The reference emu_timers are one-shot and clocked in sample ticks; these fold
    // them into generate(): `remaining` counts down one per sample.
    timer_active: [bool; 3],
    timer_remaining: [u32; 3],

    dma: Dma,

    mcieb: u16,
    mcipd: u16,

    artable: [i32; 64],
    drtable: [i32; 64],

    dsp: Dsp,

    // LFO
    plfo_tri: [i32; 256],
    plfo_sqr: [i32; 256],
    plfo_saw: [i32; 256],
    plfo_noi: [i32; 256],
    alfo_tri: [i32; 256],
    alfo_sqr: [i32; 256],
    alfo_saw: [i32; 256],
    alfo_noi: [i32; 256],
    pscales: [[i32; 256]; 8],
    ascales: [[i32; 256]; 8],

    rng: u32,
}

impl Default for Scsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Scsp {
    /// Owns the 512KB work RAM shared with the 68000.
    /// Equivalent of device_start/device_reset: EG/LFO/pan tables built, state reset.
    pub fn new() -> Self {
        let mut scsp = Scsp {
            ram: vec![0; 0x80000],
            udata: [0; 0x30 / 2],
            slots: [Slot::default(); 32],
            ringbuf: [0; 128],
            bufptr: 0,
            latched_mslc: 0,
            latched_mslc_data: 0,
            irq_tim_a: 0,
            irq_tim_bc: 0,
            irq_midi: 0,
            irq_value: 0,
            irq_asserted: false,
            midi_out_stack: [0; 32],
            midi_out_w: 0,
            midi_out_r: 0,
            midi_out_queue: std::collections::VecDeque::new(),
            midi_in_count: 0,
            midi_out_count: 0,
            midi_stack: [0; 32],
            midi_w: 0,
            midi_r: 0,
            eg_table: [0; 0x400],
            lpantable: Box::new([0; 0x10000]),
            rpantable: Box::new([0; 0x10000]),
            timpris: [0; 3],
            timcnt: [0xffff; 3],
            timer_active: [false; 3],
            timer_remaining: [0; 3],
            dma: Dma::default(),
            mcieb: 0,
            mcipd: 0,
            artable: [0; 64],
            drtable: [0; 64],
            dsp: Dsp::default(),
            plfo_tri: [0; 256],
            plfo_sqr: [0; 256],
            plfo_saw: [0; 256],
            plfo_noi: [0; 256],
            alfo_tri: [0; 256],
            alfo_sqr: [0; 256],
            alfo_saw: [0; 256],
            alfo_noi: [0; 256],
            pscales: [[0; 256]; 8],
            ascales: [[0; 256]; 8],
            rng: 0x12345678,
        };
        scsp.init();
        scsp
    }

    fn init(&mut self) {
        for i in 0..0x400 {
            let env_db = (3 * (i as i32 - 0x3ff)) as f32 / 32.0;
            let scale = (1 << SHIFT) as f32;
            self.eg_table[i] = (10.0f32.powf(env_db / 20.0) * scale) as i32;
        }

        for i in 0..0x10000usize {
            let itl = i & 0xff;
            let ipan = (i >> 0x8) & 0x1f;
            let isdl = (i >> 0xD) & 0x07;

            let mut sega_db = 0.0f32;
            if itl & 0x01 != 0 {
                sega_db -= 0.4;
            }
            if itl & 0x02 != 0 {
                sega_db -= 0.8;
            }
            if itl & 0x04 != 0 {
                sega_db -= 1.5;
            }
            if itl & 0x08 != 0 {
                sega_db -= 3.0;
            }
            if itl & 0x10 != 0 {
                sega_db -= 6.0;
            }
            if itl & 0x20 != 0 {
                sega_db -= 12.0;
            }
            if itl & 0x40 != 0 {
                sega_db -= 24.0;
            }
            if itl & 0x80 != 0 {
                sega_db -= 48.0;
            }
            let tl = 10.0f32.powf(sega_db / 20.0);

            let mut sega_db = 0.0f32;
            if ipan & 0x1 != 0 {
                sega_db -= 3.0;
            }
            if ipan & 0x2 != 0 {
                sega_db -= 6.0;
            }
            if ipan & 0x4 != 0 {
                sega_db -= 12.0;
            }
            if ipan & 0x8 != 0 {
                sega_db -= 24.0;
            }

            let pan = if (ipan & 0xf) == 0xf {
                0.0
            } else {
                10.0f32.powf(sega_db / 20.0)
            };

            let (lpan, rpan) = if ipan < 0x10 { (pan, 1.0) } else { (1.0, pan) };

            let fsdl = if isdl != 0 {
                10.0f32.powf(SDLT[isdl] / 20.0)
            } else {
                0.0
            };

            // The reference FIX(v): (u32)((1 << SHIFT) * v)
            self.lpantable[i] = ((1 << SHIFT) as f32 * (4.0 * lpan * tl * fsdl)) as u32 as i32;
            self.rpantable[i] = ((1 << SHIFT) as f32 * (4.0 * rpan * tl * fsdl)) as u32 as i32;
        }

        self.artable[0] = 0; // Infinite time
        self.drtable[0] = 0;
        self.artable[1] = 0; // Infinite time
        self.drtable[1] = 0;
        for i in 2..64 {
            let t = AR_TIMES[i]; // In ms
            if t != 0.0 {
                let step = (1023.0 * 1000.0) / (44100.0 * t);
                let scale = (1u32 << EG_SHIFT) as f64;
                self.artable[i] = (step * scale) as i32;
            } else {
                self.artable[i] = 1024 << EG_SHIFT;
            }

            let t = DR_TIMES[i]; // In ms
            let step = (1023.0 * 1000.0) / (44100.0 * t);
            let scale = (1u32 << EG_SHIFT) as f64;
            self.drtable[i] = (step * scale) as i32;
        }

        // make sure all the slots are off
        for slot in self.slots.iter_mut() {
            slot.active = false;
            slot.eg.state = EgState::Release;
        }

        self.lfo_init();
        // no "pend"
        self.udata[0x20 / 2] = 0;
        self.timcnt[0] = 0xffff;
        self.timcnt[1] = 0xffff;
        self.timcnt[2] = 0xffff;
    }

    /// Stand-in for machine().rand(). The hardware
    /// noise algorithm is unknown anyway, so any PRNG works.
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1664525).wrapping_add(1013904223);
        self.rng >> 16
    }

    fn lfo_init(&mut self) {
        for i in 0..256usize {
            let i32i = i as i32;
            // Saw
            let a = 255 - i32i;
            let p = if i < 128 { i32i } else { i32i - 256 };
            self.alfo_saw[i] = a;
            self.plfo_saw[i] = p;

            // Square
            let (a, p) = if i < 128 { (255, 127) } else { (0, -128) };
            self.alfo_sqr[i] = a;
            self.plfo_sqr[i] = p;

            // Tri
            let a = if i < 128 {
                255 - (i32i * 2)
            } else {
                (i32i * 2) - 256
            };
            let p = if i < 64 {
                i32i * 2
            } else if i < 128 {
                255 - i32i * 2
            } else if i < 192 {
                256 - i32i * 2
            } else {
                i32i * 2 - 511
            };
            self.alfo_tri[i] = a;
            self.plfo_tri[i] = p;

            // noise
            let a = (self.rand() & 0xff) as i32;
            let p = 128 - a;
            self.alfo_noi[i] = a;
            self.plfo_noi[i] = p;
        }

        for s in 0..8 {
            let limit = PSCALE[s];
            for i in -128..128 {
                // The reference CENTS(v) = LFIX(powf(2.0f, v / 1200.0f))
                let v = (limit * i as f32) / 128.0;
                self.pscales[s][(i + 128) as usize] =
                    ((1 << LFO_SHIFT) as f32 * 2.0f32.powf(v / 1200.0)) as u32 as i32;
            }
            let limit = -ASCALE[s];
            for i in 0..256 {
                // The reference DB(v) = LFIX(powf(10.0f, v / 20.0f))
                let v = (limit * i as f32) / 256.0;
                self.ascales[s][i] =
                    ((1 << LFO_SHIFT) as f32 * 10.0f32.powf(v / 20.0)) as u32 as i32;
            }
        }
    }

    // ------------------------------------------------------------------
    // 68000 register interface
    // ------------------------------------------------------------------

    /// 68000 register read for the 0x100000-0x100fff window.
    /// `offset` is a WORD offset into the window ((addr-0x100000)>>1), 0..0x800.
    pub fn read(&mut self, offset: u32) -> u16 {
        self.r16(offset * 2)
    }

    /// 68000 register write, including mem_mask byte-merge semantics.
    pub fn write(&mut self, offset: u32, data: u16, mem_mask: u16) {
        let mut tmp = self.r16(offset * 2);
        tmp = (tmp & !mem_mask) | (data & mem_mask);
        self.w16(offset * 2, tmp);
    }

    fn w16(&mut self, addr: u32, val: u16) {
        let addr = addr & 0xffff;
        if addr < 0x400 {
            let slot = (addr / 0x20) as usize;
            let a = addr & 0x1f;
            self.slots[slot].data[(a >> 1) as usize] = val;
            self.update_slot_reg(slot, a);
        } else if addr < 0x600 {
            if addr < 0x430 {
                self.udata[((addr & 0x3f) >> 1) as usize] = val;
                self.update_reg(addr & 0x3f);
            }
        } else if addr < 0x700 {
            self.ringbuf[((addr - 0x600) / 2) as usize] = val as i16;
        } else {
            // DSP
            if addr < 0x780 {
                // COEF
                self.dsp.coef[((addr - 0x700) / 2) as usize] = val as i16;
            } else if addr < 0x7c0 {
                self.dsp.madrs[((addr - 0x780) / 2) as usize] = val;
            } else if addr < 0x800 {
                // MADRS is mirrored twice
                self.dsp.madrs[((addr - 0x7c0) / 2) as usize] = val;
            } else if addr < 0xC00 {
                self.dsp.mpro[((addr - 0x800) / 2) as usize] = val;
                if addr == 0xBF0 {
                    self.dsp_start();
                }
            }
        }
    }

    fn r16(&mut self, addr: u32) -> u16 {
        let addr = addr & 0xffff;
        if addr < 0x400 {
            let slot = (addr / 0x20) as usize;
            let a = addr & 0x1f;
            // The reference UpdateSlotRegR is empty.
            self.slots[slot].data[(a >> 1) as usize]
        } else if addr < 0x600 {
            if addr < 0x430 {
                self.update_reg_r(addr & 0x3f);
                self.udata[((addr & 0x3f) >> 1) as usize]
            } else {
                0
            }
        } else if addr < 0x700 {
            self.ringbuf[((addr - 0x600) / 2) as usize] as u16
        } else {
            // DSP
            let mut v = 0u16;
            if addr < 0x780 {
                // COEF
                v = self.dsp.coef[((addr - 0x700) / 2) as usize] as u16;
            } else if addr < 0x7c0 {
                v = self.dsp.madrs[((addr - 0x780) / 2) as usize];
            } else if addr < 0x800 {
                v = self.dsp.madrs[((addr - 0x7c0) / 2) as usize];
            } else if addr < 0xC00 {
                v = self.dsp.mpro[((addr - 0x800) / 2) as usize];
            } else if addr < 0xE00 {
                if addr & 2 != 0 {
                    v = (self.dsp.temp[((addr >> 2) & 0x7f) as usize] & 0xffff) as u16;
                } else {
                    v = ((self.dsp.temp[((addr >> 2) & 0x7f) as usize] >> 16) & 0xffff) as u16;
                }
            } else if addr < 0xE80 {
                if addr & 2 != 0 {
                    v = (self.dsp.mems[((addr >> 2) & 0x1f) as usize] & 0xffff) as u16;
                } else {
                    v = ((self.dsp.mems[((addr >> 2) & 0x1f) as usize] >> 16) & 0xffff) as u16;
                }
            } else if addr < 0xEC0 {
                if addr & 2 != 0 {
                    v = (self.dsp.mixs[((addr >> 2) & 0xf) as usize] & 0xffff) as u16;
                } else {
                    v = ((self.dsp.mixs[((addr >> 2) & 0xf) as usize] >> 16) & 0xffff) as u16;
                }
            } else if addr < 0xEE0 {
                v = self.dsp.efreg[((addr - 0xec0) / 2) as usize] as u16;
            } else {
                // EXTS register(s): on Saturn this port is a parallel port wired to
                // the CD block (used by the CD player equalizer); nothing is
                // connected on Model 2A, reads return the last mixed EXTS value.
                if addr < 0xEE4 {
                    v = self.dsp.exts[((addr - 0xee0) / 2) as usize] as u16;
                }
            }
            v
        }
    }

    // ------------------------------------------------------------------
    // Slot / common register update semantics (UpdateSlotReg/UpdateReg family)
    // ------------------------------------------------------------------

    fn update_slot_reg(&mut self, s: usize, r: u32) {
        match r & 0x3f {
            0 | 1 => {
                if self.slots[s].keyonex() {
                    for s2 in 0..32 {
                        if self.slots[s2].keyonb() && self.slots[s2].eg.state == EgState::Release {
                            self.start_slot(s2);
                        }
                        if !self.slots[s2].keyonb() {
                            self.stop_slot(s2, true);
                        }
                    }
                    self.slots[s].data[0] &= !0x1000;
                }
            }
            0x10 | 0x11 => {
                self.slots[s].step = self.slot_step(s);
            }
            0xA | 0xB => {
                self.slots[s].eg.rr = self.get_dr(0, self.slots[s].rr() as i32);
                self.slots[s].eg.dl = 0x1f - self.slots[s].dl() as i32;
            }
            0x12 | 0x13 => {
                self.compute_lfo(s);
            }
            _ => {}
        }
    }

    fn update_reg(&mut self, reg: u32) {
        match reg & 0x3f {
            0x0 | 0x1 => {
                // MVOL; the reference applies it as stream output gain. Our generate()
                // re-reads MVOL every sample, so nothing to do here.
            }
            0x2 | 0x3 => {
                self.dsp.rbl = (8 * 1024) << self.rbl(); // 8 / 16 / 32 / 64 kwords
                self.dsp.rbp = self.rbp();
            }
            0x6 | 0x7 => {
                let data = (self.udata[0x6 / 2] & 0xff) as u8;
                if self.midi_out_r == self.midi_out_w {
                    // not busy, so start transmission
                    self.midi_out_queue.push_back(data);
                }
                self.midi_out_count += 1;
                self.midi_out_stack[self.midi_out_w as usize] = data;
                self.midi_out_w = (self.midi_out_w + 1) & 31;
            }
            8 | 9 => {
                // Only MSLC could be written.
                // docs claims MSLC to be 0x7800 but saturn:jikkparo doesn't agree,
                // assume doc mistake out of being 0~31 slots
                self.latched_mslc = ((self.udata[0x8 / 2] & 0xf800) >> 11) as u8;
            }
            0x12 | 0x13 => {
                self.dma.dmea = (self.udata[0x12 / 2] & 0xfffe) as u32 | (self.dma.dmea & 0xf0000);
            }
            0x14 | 0x15 => {
                self.dma.dmea =
                    (((self.udata[0x14 / 2] & 0xf000) as u32) << 4) | (self.dma.dmea & 0xfffe);
                self.dma.drga = self.udata[0x14 / 2] & 0x0ffe;
            }
            0x16 | 0x17 => {
                self.dma.dtlg = self.udata[0x16 / 2] & 0x0ffe;
                self.dma.ddir = self.udata[0x16 / 2] & 0x2000 != 0;
                self.dma.dgate = self.udata[0x16 / 2] & 0x4000 != 0;
                if self.udata[0x16 / 2] & 0x1000 != 0 {
                    // dexe
                    self.exec_dma();
                }
            }
            0x18 | 0x19 => {
                // A board with no interrupt line wired guards these; our IRQ output is
                // always connected (Model 2A wires it to the 68000).
                self.timer_setup(0);
            }
            0x1a | 0x1b => {
                self.timer_setup(1);
            }
            0x1c | 0x1d => {
                self.timer_setup(2);
            }
            0x1e | 0x1f => {
                // SCIEB
                self.check_pending_irq();
            }
            0x20 | 0x21 => {
                // SCIPD: no effect
            }
            0x22 | 0x23 => {
                // SCIRE
                self.udata[0x20 / 2] &= !self.udata[0x22 / 2];
                self.reset_interrupts();

                // behavior from real hardware: if you SCIRE a timer that's expired,
                // it'll immediately pop up again in SCIPD. cfr. saturn:sakurat
                if self.timcnt[0] == 0xffff {
                    self.udata[0x20 / 2] |= 0x40;
                }
                if self.timcnt[1] == 0xffff {
                    self.udata[0x20 / 2] |= 0x80;
                }
                if self.timcnt[2] == 0xffff {
                    self.udata[0x20 / 2] |= 0x100;
                }
            }
            0x24..=0x29 => {
                self.irq_tim_a = self.decode_sci(SCITMA) as u32;
                self.irq_tim_bc = self.decode_sci(SCITMB) as u32;
                self.irq_midi = self.decode_sci(SCIMID) as u32;
            }
            0x2a | 0x2b => {
                self.mcieb = self.udata[0x2a / 2];
                self.main_check_pending_irq(0);
            }
            0x2c | 0x2d => {
                if self.udata[0x2c / 2] & 0x20 != 0 {
                    self.main_check_pending_irq(0x20);
                }
            }
            0x2e | 0x2f => {
                self.mcipd &= !self.udata[0x2e / 2];
                self.main_check_pending_irq(0);
            }
            _ => {}
        }
    }

    fn update_reg_r(&mut self, reg: u32) {
        match reg & 0x3f {
            4 | 5 => {
                let mut v = self.udata[0x4 / 2];
                v &= 0xff00;
                v |= self.midi_stack[self.midi_r as usize] as u16;
                if self.midi_r != self.midi_w {
                    self.midi_r = (self.midi_r + 1) & 31;
                }
                if self.midi_r == self.midi_w {
                    // if the input FIFO is empty, clear the IRQ
                    self.irq_clear(self.irq_midi);
                    self.udata[0x20 / 2] &= !8;
                }
                self.udata[0x4 / 2] = v;
            }
            8 | 9 => {
                self.udata[0x8 / 2] = self.latched_mslc_data;
            }
            0x18..=0x1d => {}
            0x2a | 0x2b => {
                self.udata[0x2a / 2] = self.mcieb;
            }
            0x2c | 0x2d => {
                self.udata[0x2c / 2] = self.mcipd;
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Interrupt handling (CheckPendingIRQ/DecodeSCI/ResetInterrupts)
    // ------------------------------------------------------------------

    fn decode_sci(&self, irq: u32) -> u8 {
        let mut sci = 0u8;
        let v = if self.scilv0() & (1 << irq) != 0 {
            1
        } else {
            0
        };
        sci |= v;
        let v = if self.scilv1() & (1 << irq) != 0 {
            1
        } else {
            0
        };
        sci |= v << 1;
        let v = if self.scilv2() & (1 << irq) != 0 {
            1
        } else {
            0
        };
        sci |= v << 2;
        sci
    }

    /// The SCSP drives a single 3-bit
    /// interrupt level onto the 68000 input line of that number
    /// (model2.cpp scsp_irq -> set_input_line(offset, data)).
    fn irq_assert(&mut self, value: u32) {
        self.irq_value = value;
        self.irq_asserted = true;
    }

    fn irq_clear(&mut self, value: u32) {
        self.irq_value = value;
        self.irq_asserted = false;
    }

    fn check_pending_irq(&mut self) {
        let mut pend = self.udata[0x20 / 2] as u32;
        let en = self.udata[0x1e / 2] as u32;
        if self.midi_w != self.midi_r {
            self.udata[0x20 / 2] |= 8;
            pend |= 8;
        }
        if pend == 0 {
            return;
        }
        if pend & 0x40 != 0 && en & 0x40 != 0 {
            self.irq_assert(self.irq_tim_a);
            return;
        }
        if pend & 0x80 != 0 && en & 0x80 != 0 {
            self.irq_assert(self.irq_tim_bc);
            return;
        }
        if pend & 0x100 != 0 && en & 0x100 != 0 {
            self.irq_assert(self.irq_tim_bc);
            return;
        }
        if pend & 8 != 0 && en & 8 != 0 {
            self.irq_assert(self.irq_midi);
            return;
        }
        self.irq_clear(0);
    }

    /// On a Saturn this would also drive the SH-2
    /// side); that output is not wired up on Model 2, so only MCIPD itself
    /// (readable at 0x10042c) is tracked here.
    fn main_check_pending_irq(&mut self, irq_type: u16) {
        self.mcipd |= irq_type;
    }

    fn reset_interrupts(&mut self) {
        let reset = self.udata[0x22 / 2] as u32;

        if reset & 0x40 != 0 {
            self.irq_clear(self.irq_tim_a);
        }
        if reset & 0x180 != 0 {
            self.irq_clear(self.irq_tim_bc);
        }
        if reset & 0x8 != 0 {
            self.irq_clear(self.irq_midi);
        }

        self.check_pending_irq();
    }

    /// Current SCSP->68000 interrupt line states, lines 0..8. The SCSP has a
    /// single interrupt output carrying a 3-bit level decoded from SCIPD &
    /// SCIEB through SCILV0/1/2 (CheckPendingIRQ/DecodeSCI); line N of the
    /// returned array is asserted when the output is asserted with level N.
    pub fn irq_lines(&self) -> [bool; 8] {
        let mut lines = [false; 8];
        if self.irq_asserted && self.irq_value < 8 {
            lines[self.irq_value as usize] = true;
        }
        lines
    }

    // ------------------------------------------------------------------
    // Timers A/B/C
    // ------------------------------------------------------------------

    fn timer_setup(&mut self, t: usize) {
        let reg = self.udata[(0x18 / 2) + t];
        self.timpris[t] = 1 << ((reg >> 8) & 0x7);
        self.timcnt[t] = ((reg & 0xff) as i32) << 8;

        if (reg & 0xff) != 255 {
            let time = (CLOCK / self.timpris[t]) / (255 - (reg & 0xff) as u32);
            if time != 0 {
                // The reference arms a one-shot emu_timer with a period of 512/time
                // seconds. At the 44100 Hz sample rate (CLOCK/512) that is
                // ceil(CLOCK/time) samples per generate() call.
                self.timer_remaining[t] = CLOCK.div_ceil(time);
                self.timer_active[t] = true;
            }
        }
    }

    fn timers_tick(&mut self) {
        for t in 0..3 {
            if self.timer_active[t] {
                self.timer_remaining[t] -= 1;
                if self.timer_remaining[t] == 0 {
                    self.timer_active[t] = false;
                    self.timer_fire(t);
                }
            }
        }
    }

    fn timer_fire(&mut self, t: usize) {
        self.timcnt[t] = 0xFFFF;
        match t {
            // timerA_cb
            0 => {
                self.udata[0x20 / 2] |= 0x40;
                self.udata[0x18 / 2] &= 0xff00;
                self.udata[0x18 / 2] |= (self.timcnt[0] >> 8) as u16;
                self.check_pending_irq();
                self.main_check_pending_irq(0x40);
            }
            // timerB_cb
            1 => {
                self.udata[0x20 / 2] |= 0x80;
                self.udata[0x1a / 2] &= 0xff00;
                self.udata[0x1a / 2] |= (self.timcnt[1] >> 8) as u16;
                self.check_pending_irq();
            }
            // timerC_cb
            _ => {
                self.udata[0x20 / 2] |= 0x100;
                self.udata[0x1c / 2] &= 0xff00;
                self.udata[0x1c / 2] |= (self.timcnt[2] >> 8) as u16;
                self.check_pending_irq();
            }
        }
    }

    // ------------------------------------------------------------------
    // DMA
    // ------------------------------------------------------------------

    // TODO: this needs to be timer-ized
    fn exec_dma(&mut self) {
        // Copy the dma values in a temp storage for resuming later
        // (DMA *can't* overwrite its parameters).
        let mut tmp_dma = [0u16; 3];
        if !self.dma.ddir {
            for i in 0..3 {
                tmp_dma[i] = self.udata[(0x12 + (i * 2)) / 2];
            }
        }

        // note: we don't use space.read_word / write_word because it can happen
        // that SH-2 enables the DMA instead of m68k.
        // TODO: don't know if params auto-updates, I guess not...
        let mut i = 0u32;
        if self.dma.ddir {
            if self.dma.dgate {
                // The reference pops a "Check: SCSP DMA DGATE enabled" message here
                while i < self.dma.dtlg as u32 {
                    self.write_word_ram(self.dma.dmea, 0);
                    self.dma.dmea = self.dma.dmea.wrapping_add(2);
                    i += 2;
                }
            } else {
                while i < self.dma.dtlg as u32 {
                    let tmp = self.r16(self.dma.drga as u32);
                    self.write_word_ram(self.dma.dmea, tmp);
                    self.dma.dmea = self.dma.dmea.wrapping_add(2);
                    self.dma.drga = self.dma.drga.wrapping_add(2);
                    i += 2;
                }
            }
        } else if self.dma.dgate {
            // The reference pops a "Check: SCSP DMA DGATE enabled" message here
            while i < self.dma.dtlg as u32 {
                self.w16(self.dma.drga as u32, 0);
                self.dma.drga = self.dma.drga.wrapping_add(2);
                i += 2;
            }
        } else {
            while i < self.dma.dtlg as u32 {
                let tmp = self.read_word_ram(self.dma.dmea);
                self.w16(self.dma.drga as u32, tmp);
                self.dma.dmea = self.dma.dmea.wrapping_add(2);
                self.dma.drga = self.dma.drga.wrapping_add(2);
                i += 2;
            }
        }

        // Resume the values
        if !self.dma.ddir {
            for i in 0..3 {
                self.udata[(0x12 + (i * 2)) / 2] = tmp_dma[i];
            }
        }

        // Job done
        self.udata[0x16 / 2] &= !0x1000;
        // request a dma end irq
        if self.udata[0x1e / 2] & 0x10 != 0 {
            // The DMA-complete interrupt is a pulse; we represent
            // it as a level assert that stays until the next irq clear (the
            // board glue edge-detects it).
            let v = self.decode_sci(SCIDMA) as u32;
            self.irq_assert(v);
        }
    }

    // ------------------------------------------------------------------
    // MIDI
    // ------------------------------------------------------------------

    /// One complete MIDI byte received from the main board's UART.
    /// Number of slots currently playing, for bring-up diagnostics.
    /// (SCIEB enable, SCIPD pending, MIDI irq level, asserted level) --
    /// bring-up view of the interrupt path.
    pub fn irq_debug(&self) -> (u16, u16, u32, i32) {
        (
            self.udata[0x1e / 2],
            self.udata[0x20 / 2],
            self.irq_midi,
            if self.irq_asserted {
                self.irq_value as i32
            } else {
                -1
            },
        )
    }

    pub fn active_voices(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }

    pub fn midi_in(&mut self, byte: u8) {
        self.midi_in_count += 1;
        self.midi_stack[self.midi_w as usize] = byte;
        self.midi_w = (self.midi_w + 1) & 31;
        self.check_pending_irq();
    }

    /// Bytes the SCSP wants to send back to the main board. Popping a byte
    /// completes its transmission: the out-FIFO read
    /// pointer advances and the next queued byte starts transmitting.
    pub fn midi_out_pop(&mut self) -> Option<u8> {
        let b = self.midi_out_queue.pop_front()?;
        self.midi_out_r = (self.midi_out_r + 1) & 31;
        // if buffer not empty, transmit next byte
        if self.midi_out_r != self.midi_out_w {
            self.midi_out_queue
                .push_back(self.midi_out_stack[self.midi_out_r as usize]);
        }
        Some(b)
    }

    // ------------------------------------------------------------------
    // Common register bit fields
    // ------------------------------------------------------------------

    #[inline]
    fn dac18b(&self) -> bool {
        self.udata[0] & 0x0100 != 0
    }
    #[inline]
    fn mvol(&self) -> u32 {
        (self.udata[0] & 0x000F) as u32
    }
    #[inline]
    fn rbl(&self) -> u32 {
        ((self.udata[1] >> 0x7) & 0x0003) as u32
    }
    #[inline]
    fn rbp(&self) -> u32 {
        (self.udata[1] & 0x003F) as u32
    }
    #[inline]
    fn scilv0(&self) -> u32 {
        (self.udata[0x24 / 2] & 0xff) as u32
    }
    #[inline]
    fn scilv1(&self) -> u32 {
        (self.udata[0x26 / 2] & 0xff) as u32
    }
    #[inline]
    fn scilv2(&self) -> u32 {
        (self.udata[0x28 / 2] & 0xff) as u32
    }

    // ------------------------------------------------------------------
    // Sound RAM access
    // ------------------------------------------------------------------

    #[inline]
    fn read_byte_ram(&self, addr: u32) -> u8 {
        self.ram[(addr & 0x7ffff) as usize]
    }

    #[inline]
    fn read_word_ram(&self, addr: u32) -> u16 {
        ((self.read_byte_ram(addr) as u16) << 8) | self.read_byte_ram(addr.wrapping_add(1)) as u16
    }

    #[inline]
    fn write_word_ram(&mut self, addr: u32, val: u16) {
        self.ram[(addr & 0x7ffff) as usize] = (val >> 8) as u8;
        self.ram[(addr.wrapping_add(1) & 0x7ffff) as usize] = (val & 0xff) as u8;
    }

    // ------------------------------------------------------------------
    // Envelope generator
    // ------------------------------------------------------------------

    fn get_ar(&self, base: i32, r: i32) -> i32 {
        let rate = base + (r << 1);
        self.artable[rate.clamp(0, 63) as usize]
    }

    fn get_dr(&self, base: i32, r: i32) -> i32 {
        let rate = base + (r << 1);
        self.drtable[rate.clamp(0, 63) as usize]
    }

    fn compute_eg(&mut self, sl: usize) {
        let octave = (self.slots[sl].oct() ^ 8) as i32 - 8;
        let rate = if self.slots[sl].krs() != 0xf {
            octave + 2 * self.slots[sl].krs() as i32 + ((self.slots[sl].fns() >> 9) & 1) as i32
        } else {
            0
        };

        self.slots[sl].eg.volume = 0x17F << EG_SHIFT;
        self.slots[sl].eg.ar = self.get_ar(rate, self.slots[sl].ar() as i32);
        self.slots[sl].eg.d1r = self.get_dr(rate, self.slots[sl].d1r() as i32);
        self.slots[sl].eg.d2r = self.get_dr(rate, self.slots[sl].d2r() as i32);
        self.slots[sl].eg.rr = self.get_dr(rate, self.slots[sl].rr() as i32);
        self.slots[sl].eg.dl = 0x1f - self.slots[sl].dl() as i32;
        self.slots[sl].eg.eghold = self.slots[sl].eghold();
    }

    fn eg_update(&mut self, sl: usize) -> i32 {
        match self.slots[sl].eg.state {
            EgState::Attack => {
                self.slots[sl].eg.volume += self.slots[sl].eg.ar;
                if self.slots[sl].eg.volume >= 0x3ff << EG_SHIFT {
                    if !self.slots[sl].lpslnk() {
                        self.slots[sl].eg.state = EgState::Decay1;
                        if self.slots[sl].eg.d1r >= 1024 << EG_SHIFT {
                            // Skip SCSP_DECAY1, go directly to SCSP_DECAY2
                            self.slots[sl].eg.state = EgState::Decay2;
                        }
                    }
                    self.slots[sl].eg.volume = 0x3ff << EG_SHIFT;
                }
                if self.slots[sl].eg.eghold {
                    return 0x3ff << (SHIFT - 10);
                }
            }
            EgState::Decay1 => {
                self.slots[sl].eg.volume -= self.slots[sl].eg.d1r;
                if self.slots[sl].eg.volume <= 0 {
                    self.slots[sl].eg.volume = 0;
                }
                if self.slots[sl].eg.volume >> (EG_SHIFT + 5) <= self.slots[sl].eg.dl {
                    self.slots[sl].eg.state = EgState::Decay2;
                }
            }
            EgState::Decay2 => {
                if self.slots[sl].d2r() == 0 {
                    return (self.slots[sl].eg.volume >> EG_SHIFT) << (SHIFT - 10);
                }
                self.slots[sl].eg.volume -= self.slots[sl].eg.d2r;
                if self.slots[sl].eg.volume <= 0 {
                    self.slots[sl].eg.volume = 0;
                }
            }
            EgState::Release => {
                self.slots[sl].eg.volume -= self.slots[sl].eg.rr;
                if self.slots[sl].eg.volume <= 0 {
                    self.slots[sl].eg.volume = 0;
                    self.stop_slot(sl, false);
                }
            }
        }
        (self.slots[sl].eg.volume >> EG_SHIFT) << (SHIFT - 10)
    }

    // ------------------------------------------------------------------
    // Slot key on/off, pitch step
    // ------------------------------------------------------------------

    /// The reference Step(): pitch step in 24.8 fixed point.
    fn slot_step(&self, sl: usize) -> u32 {
        let octave = (self.slots[sl].oct() ^ 8) as i32 - 8 + SHIFT as i32 - 10;
        let mut fn_ = self.slots[sl].fns() + (1 << 10);
        if octave >= 0 {
            fn_ <<= octave;
        } else {
            fn_ >>= -octave;
        }
        fn_
    }

    fn compute_lfo(&mut self, sl: usize) {
        if self.slots[sl].plfos() != 0 {
            self.lfo_compute_step(sl, false);
        }
        if self.slots[sl].alfos() != 0 {
            self.lfo_compute_step(sl, true);
        }
    }

    fn start_slot(&mut self, sl: usize) {
        self.slots[sl].active = true;
        self.slots[sl].cur_addr = 0;
        self.slots[sl].nxt_addr = 1 << SHIFT;
        self.slots[sl].step = self.slot_step(sl);
        self.compute_eg(sl);
        self.slots[sl].eg.state = EgState::Attack;
        self.slots[sl].eg.volume = 0x17F << EG_SHIFT;
        self.slots[sl].backwards = false;

        self.compute_lfo(sl);
    }

    fn stop_slot(&mut self, sl: usize, keyoff: bool) {
        if keyoff {
            self.slots[sl].eg.state = EgState::Release;
        } else {
            self.slots[sl].active = false;
        }
        self.slots[sl].data[0] &= !0x800;
    }

    // ------------------------------------------------------------------
    // LFO
    // ------------------------------------------------------------------

    fn plfo_step(&mut self, sl: usize) -> i32 {
        let lfo = &mut self.slots[sl].plfo;
        lfo.phase = lfo.phase.wrapping_add(lfo.phase_step as u16);
        let idx = (lfo.phase >> LFO_SHIFT) as usize;
        let p = match lfo.wave {
            0 => self.plfo_saw[idx],
            1 => self.plfo_sqr[idx],
            2 => self.plfo_tri[idx],
            _ => self.plfo_noi[idx],
        };
        let scale = lfo.scale as usize;
        let pi = (p + 128) as usize;
        // The noise waveform can yield p == 128, which indexes one past the
        // 256-entry scale row. The reference simply reads the adjacent memory (the next
        // PSCALES row, or ASCALES[0] for scale 7); replicate that layout.
        let p = if pi < 256 {
            self.pscales[scale][pi]
        } else if scale < 7 {
            self.pscales[scale + 1][0]
        } else {
            self.ascales[0][0]
        };
        p << (SHIFT - LFO_SHIFT)
    }

    fn alfo_step(&mut self, sl: usize) -> i32 {
        let lfo = &mut self.slots[sl].alfo;
        lfo.phase = lfo.phase.wrapping_add(lfo.phase_step as u16);
        let idx = (lfo.phase >> LFO_SHIFT) as usize;
        let p = match lfo.wave {
            0 => self.alfo_saw[idx],
            1 => self.alfo_sqr[idx],
            2 => self.alfo_tri[idx],
            _ => self.alfo_noi[idx],
        };
        let p = self.ascales[lfo.scale as usize][p as usize];
        p << (SHIFT - LFO_SHIFT)
    }

    fn lfo_compute_step(&mut self, sl: usize, alfo: bool) {
        let lfof = self.slots[sl].lfof() as usize;
        let (lfows, lfos) = if alfo {
            (self.slots[sl].alfows(), self.slots[sl].alfos())
        } else {
            (self.slots[sl].plfows(), self.slots[sl].plfos())
        };
        let step = LFO_FREQ[lfof] * 256.0 / 44100.0;
        let lfo = if alfo {
            &mut self.slots[sl].alfo
        } else {
            &mut self.slots[sl].plfo
        };
        lfo.phase_step = ((1 << LFO_SHIFT) as f32 * step) as u32;
        lfo.wave = lfows as u8;
        lfo.scale = lfos as u8;
    }

    // ------------------------------------------------------------------
    // Slot processing
    // ------------------------------------------------------------------

    fn update_slot(&mut self, sl: usize) -> i32 {
        if self.slots[sl].ssctl() == 3 {
            // manual says cannot be used
            return 0;
        }

        let mut sample: i32;
        let mut step = self.slots[sl].step as i32;
        let mut addr1: u32; // current and next sample addresses
        let mut addr2: u32;

        if self.slots[sl].plfos() != 0 {
            step = step.wrapping_mul(self.plfo_step(sl));
            step >>= SHIFT;
        }

        if self.slots[sl].pcm8b() {
            addr1 = self.slots[sl].cur_addr >> SHIFT;
            addr2 = self.slots[sl].nxt_addr >> SHIFT;
        } else {
            addr1 = (self.slots[sl].cur_addr >> (SHIFT - 1)) & !1;
            addr2 = (self.slots[sl].nxt_addr >> (SHIFT - 1)) & !1;
        }

        let mdl = self.slots[sl].mdl();
        let mdxsl = self.slots[sl].mdxsl();
        let mdysl = self.slots[sl].mdysl();
        if mdl != 0 || mdxsl != 0 || mdysl != 0 {
            let mut smp = (self.ringbuf[((self.bufptr as u32 + mdxsl) & 63) as usize] as i32
                + self.ringbuf[((self.bufptr as u32 + mdysl) & 63) as usize] as i32)
                / 2;

            smp <<= 0xA; // associate cycle with 1024
            smp >>= 0x1A - mdl; // ex. for MDL=0xF, sample range corresponds to +/- 64 pi (32=2^5 cycles) so shift by 11 (16-5 == 0x1A-0xF)
            if !self.slots[sl].pcm8b() {
                smp <<= 1;
            }

            addr1 = addr1.wrapping_add(smp as u32);
            addr2 = addr2.wrapping_add(smp as u32);
        }

        let ssctl = self.slots[sl].ssctl();
        if ssctl == 0 {
            // External DRAM data
            let sa = self.slots[sl].sa();
            if self.slots[sl].pcm8b() {
                // 8 bit signed
                let p1 = self.read_byte_ram(sa.wrapping_add(addr1)) as i8 as i32;
                let p2 = self.read_byte_ram(sa.wrapping_add(addr2)) as i8 as i32;
                let fpart = (self.slots[sl].cur_addr & ((1 << SHIFT) - 1)) as i32;
                let s = (p1 << 8) * ((1 << SHIFT) - fpart) + (p2 << 8) * fpart;
                sample = s >> SHIFT;
            } else {
                // 16 bit signed
                let p1 = self.read_word_ram(sa.wrapping_add(addr1)) as i16 as i32;
                let p2 = self.read_word_ram(sa.wrapping_add(addr2)) as i16 as i32;
                let fpart = (self.slots[sl].cur_addr & ((1 << SHIFT) - 1)) as i32;
                let s = p1 * ((1 << SHIFT) - fpart) + p2 * fpart;
                sample = s >> SHIFT;
            }
        } else if ssctl == 1 {
            // Internally generated data (Noise). Unknown algorithm.
            sample = (self.rand() & 0xffff) as u16 as i16 as i32;
        } else {
            // Internally generated data (All 0)
            sample = 0;
        }

        let sbctl = self.slots[sl].sbctl();
        if sbctl & 0x1 != 0 {
            sample ^= 0x7FFF;
        }
        if sbctl & 0x2 != 0 {
            sample = ((sample ^ 0x8000) as u16) as i16 as i32;
        }

        let step_u = step as u32;
        if self.slots[sl].backwards {
            self.slots[sl].cur_addr = self.slots[sl].cur_addr.wrapping_sub(step_u);
        } else {
            self.slots[sl].cur_addr = self.slots[sl].cur_addr.wrapping_add(step_u);
        }
        self.slots[sl].nxt_addr = self.slots[sl].cur_addr.wrapping_add(1 << SHIFT);

        addr1 = self.slots[sl].cur_addr >> SHIFT;
        addr2 = self.slots[sl].nxt_addr >> SHIFT;

        let lsa = self.slots[sl].lsa();
        let lea = self.slots[sl].lea();

        if addr1 >= lsa
            && !self.slots[sl].backwards
            && self.slots[sl].lpslnk()
            && self.slots[sl].eg.state == EgState::Attack
        {
            self.slots[sl].eg.state = EgState::Decay1;
        }

        for addr_select in 0..2 {
            let a = if addr_select == 0 { addr1 } else { addr2 };
            match self.slots[sl].lpctl() {
                0 => {
                    // no loop
                    if a >= lsa && a >= lea {
                        self.stop_slot(sl, false);
                    }
                }
                1 => {
                    // normal loop
                    if a >= lea {
                        let cur = self.slot_addr(sl, addr_select);
                        let rem_addr = cur.wrapping_sub(lea << SHIFT);
                        self.set_slot_addr(sl, addr_select, (lsa << SHIFT).wrapping_add(rem_addr));
                    }
                }
                2 => {
                    // reverse loop
                    let cur = self.slot_addr(sl, addr_select);
                    if a >= lsa && !self.slots[sl].backwards {
                        let rem_addr = cur.wrapping_sub(lsa << SHIFT);
                        self.set_slot_addr(sl, addr_select, (lea << SHIFT).wrapping_sub(rem_addr));
                        self.slots[sl].backwards = true;
                    } else if (a < lsa || (cur & 0x80000000) != 0) && self.slots[sl].backwards {
                        let rem_addr = (lsa << SHIFT).wrapping_sub(cur);
                        self.set_slot_addr(sl, addr_select, (lea << SHIFT).wrapping_sub(rem_addr));
                    }
                }
                _ => {
                    // 3: ping-pong
                    let cur = self.slot_addr(sl, addr_select);
                    if a >= lea {
                        // reached end, reverse till start
                        let rem_addr = cur.wrapping_sub(lea << SHIFT);
                        self.set_slot_addr(sl, addr_select, (lea << SHIFT).wrapping_sub(rem_addr));
                        self.slots[sl].backwards = true;
                    } else if (a < lsa || (cur & 0x80000000) != 0) && self.slots[sl].backwards {
                        // reached start or negative
                        let rem_addr = (lsa << SHIFT).wrapping_sub(cur);
                        self.set_slot_addr(sl, addr_select, (lsa << SHIFT).wrapping_add(rem_addr));
                        self.slots[sl].backwards = false;
                    }
                }
            }
        }

        if !self.slots[sl].sdir() {
            if self.slots[sl].alfos() != 0 {
                sample = sample.wrapping_mul(self.alfo_step(sl));
                sample >>= SHIFT;
            }

            if self.slots[sl].eg.state == EgState::Attack {
                sample = (sample * self.eg_update(sl)) >> SHIFT;
            } else {
                // the returned envelope level is 0..0x3ff << (SHIFT-10) in every
                // reachable state; the mask is a bounds guard, not a behavior change
                let eg = self.eg_update(sl) >> (SHIFT - 10);
                sample = (sample * self.eg_table[(eg & 0x3ff) as usize]) >> SHIFT;
            }
        }

        if !self.slots[sl].stwinh() {
            let enc: u16 = if !self.slots[sl].sdir() {
                (self.slots[sl].tl() as u16) | (0x7 << 0xd)
            } else {
                0x7 << 0xd
            };
            // with SCSP_FM_DELAY == 0, RBUFDST points at RINGBUF[BUFPTR]
            self.ringbuf[self.bufptr as usize] =
                ((sample * self.lpantable[enc as usize]) >> (SHIFT + 1)) as i16;
        }

        sample
    }

    #[inline]
    fn slot_addr(&self, sl: usize, select: usize) -> u32 {
        if select == 0 {
            self.slots[sl].cur_addr
        } else {
            self.slots[sl].nxt_addr
        }
    }

    #[inline]
    fn set_slot_addr(&mut self, sl: usize, select: usize, v: u32) {
        if select == 0 {
            self.slots[sl].cur_addr = v;
        } else {
            self.slots[sl].nxt_addr = v;
        }
    }

    // ------------------------------------------------------------------
    // Master sample generation
    // ------------------------------------------------------------------

    /// Render one stereo output sample at 44100 Hz (SCSP clock 22579200/512).
    /// Returns the post-master-volume sample in 16-bit full scale.
    pub fn generate(&mut self) -> (i32, i32) {
        let mut smpl: i32 = 0;
        let mut smpr: i32 = 0;

        for sl in 0..32 {
            // SCSP_FM_DELAY is 0: RBUFDST points directly at RINGBUF[BUFPTR].
            if self.slots[sl].active {
                let sample = self.update_slot(sl);

                // SDIR ("sound direct") sends the raw sample straight to the output,
                // bypassing the envelope generator AND the TL attenuator (the EG/ALFO
                // bypass is handled in UpdateSlot). BOTH downstream mixes -- the DSP
                // input feed here and the direct-output mix below -- must therefore
                // zero TL when SDIR is set, otherwise a slot programmed with SDIR=1 +
                // a large TL is wrongly muted.
                // (Flash Beats keys its SFX with SDIR=1, TL=0xff = -95 dB; in
                // particular its in-game/"Voice" SFX route only through the DSP
                // (DISDL=0, IMXL>0), so without this the effect path is starved to
                // near-silence.)
                let eff_tl = if self.slots[sl].sdir() {
                    0
                } else {
                    self.slots[sl].tl() as u16
                };
                let enc = eff_tl | ((self.slots[sl].imxl() as u16) << 0xd);
                let dsp_sample = (sample * self.lpantable[enc as usize]) >> (SHIFT - 2);
                let isel = self.slots[sl].isel() as usize;
                let imxl = self.slots[sl].imxl();
                self.dsp_set_sample(dsp_sample, isel, imxl);
                let enc = eff_tl
                    | ((self.slots[sl].dipan() as u16) << 0x8)
                    | ((self.slots[sl].disdl() as u16) << 0xd);
                smpl += (sample * self.lpantable[enc as usize]) >> SHIFT;
                smpr += (sample * self.rpantable[enc as usize]) >> SHIFT;
            }

            self.bufptr = (self.bufptr + 1) & 63;
        }

        self.dsp_step();

        for i in 0..16 {
            if self.slots[i].efsdl() != 0 {
                let enc = ((self.slots[i].efpan() as u16) << 0x8)
                    | ((self.slots[i].efsdl() as u16) << 0xd);
                smpl += ((self.dsp.efreg[i] as i32) * self.lpantable[enc as usize]) >> SHIFT;
                smpr += ((self.dsp.efreg[i] as i32) * self.rpantable[enc as usize]) >> SHIFT;
            }
        }

        for i in 0..2 {
            // EFSDL, EFPAN of slots 16/17 for EXTS0/1
            if self.slots[i + 16].efsdl() != 0 {
                // no external input stream is connected on Model 2A
                // (EXTS is the CDDA input on Saturn), so this reads as silence
                self.dsp.exts[i] = 0;
                let enc = ((self.slots[i + 16].efpan() as u16) << 0x8)
                    | ((self.slots[i + 16].efsdl() as u16) << 0xd);
                smpl += ((self.dsp.exts[i] as i32) * self.lpantable[enc as usize]) >> SHIFT;
                smpr += ((self.dsp.exts[i] as i32) * self.rpantable[enc as usize]) >> SHIFT;
            }
        }

        // The reference: stream.put_int_clamp(..., max), then the stream applies the
        // MVOL output gain. We return the gained sample in 16-bit full scale.
        let (fl, fr);
        if self.dac18b() {
            fl = smpl.clamp(-131072, 131071) as f32 / 131072.0;
            fr = smpr.clamp(-131072, 131071) as f32 / 131072.0;
        } else {
            fl = (smpl >> 2).clamp(-32768, 32767) as f32 / 32768.0;
            fr = (smpr >> 2).clamp(-32768, 32767) as f32 / 32768.0;
        }
        let gain = self.mvol() as f32 / 15.0;
        let l = (fl * gain * 32768.0) as i32;
        let r = (fr * gain * 32768.0) as i32;

        // The reference emu_timers fire in sample ticks; count them down here so the
        // timer IRQ bits set on the same schedule.
        self.timers_tick();

        // MSLC     | CA   |SGC|EG
        // f e d c b a 9 8 7 6 5 4 3 2 1 0
        //
        // latch the new MSLC, updates every 44.1 kHz
        // cfr. vstriker (GK reflecting ball with heavy shots) and srallyc
        // (PowerGames BGM bleeps at end). The reference latches once per stream update;
        // here we latch once per sample.
        let mslc = self.latched_mslc as usize;
        let sgc = (self.slots[mslc].eg.state as u32) & 3;
        let ca = (self.slots[mslc].cur_addr >> (SHIFT + 12)) & 0xf;
        let eg = (0x1f - (self.slots[mslc].eg.volume >> (EG_SHIFT + 5))) & 0x1f;
        // NOTE: according to the manual MSLC is write only, CA, SGC and EG read only.
        // saturn:toughtrk will hang on Human logo otherwise
        self.latched_mslc_data = ((ca << 7) | (sgc << 5) | eg as u32) as u16;

        (l, r)
    }

    // ------------------------------------------------------------------
    // DSP
    // ------------------------------------------------------------------

    fn dsp_set_sample(&mut self, sample: i32, sel: usize, _mxl: u32) {
        // The reference: MIXS[SEL] += sample; the MXL argument is unused there too.
        self.dsp.mixs[sel] = self.dsp.mixs[sel].wrapping_add(sample);
    }

    fn dsp_step(&mut self) {
        if self.dsp.stopped {
            return;
        }

        self.dsp.efreg = [0; 16];

        let mut acc: i32 = 0; // 26 bit
        let mut memval: i32 = 0;
        let mut frc_reg: i32 = 0; // 13 bit
        let mut y_reg: i32 = 0; // 24 bit
        let mut adrs_reg: u32 = 0; // 13 bit

        for step in 0..self.dsp.last_step {
            let iptr = [
                self.dsp.mpro[step * 4],
                self.dsp.mpro[step * 4 + 1],
                self.dsp.mpro[step * 4 + 2],
                self.dsp.mpro[step * 4 + 3],
            ];

            let tra = ((iptr[0] >> 8) & 0x7f) as u32;
            let twt = (iptr[0] >> 7) & 0x01;
            let twa = (iptr[0] & 0x7f) as u32;

            let xsel = (iptr[1] >> 15) & 0x01;
            let ysel = (iptr[1] >> 13) & 0x03;
            let ira = ((iptr[1] >> 6) & 0x3f) as u32;
            let iwt = (iptr[1] >> 5) & 0x01;
            let iwa = (iptr[1] & 0x1f) as u32;

            let table = (iptr[2] >> 15) & 0x01;
            let mwt = (iptr[2] >> 14) & 0x01;
            let mrd = (iptr[2] >> 13) & 0x01;
            let ewt = (iptr[2] >> 12) & 0x01;
            let ewa = ((iptr[2] >> 8) & 0x0f) as usize;
            let adrl = (iptr[2] >> 7) & 0x01;
            let frcl = (iptr[2] >> 6) & 0x01;
            let shift = (iptr[2] >> 4) & 0x03;
            let yrl = (iptr[2] >> 3) & 0x01;
            let negb = (iptr[2] >> 2) & 0x01;
            let zero = (iptr[2] >> 1) & 0x01;
            let bsel = iptr[2] & 0x01;

            let nofl = (iptr[3] >> 15) & 0x01; //????
            let coef = ((iptr[3] >> 9) & 0x3f) as usize;

            let masa = ((iptr[3] >> 2) & 0x1f) as usize; //???
            let adreb = (iptr[3] >> 1) & 0x01;
            let nxadr = iptr[3] & 0x01;

            // operations are done at 24 bit precision

            // INPUTS RW
            let mut inputs: i32; // 24-bit
            if ira <= 0x1f {
                inputs = self.dsp.mems[ira as usize];
            } else if ira <= 0x2F {
                inputs = self.dsp.mixs[(ira - 0x20) as usize] << 4; // MIXS is 20 bit
            } else if ira <= 0x31 {
                inputs = (self.dsp.exts[(ira - 0x30) as usize] as i32) << 8; // EXTS is 16 bit
            } else {
                return;
            }

            inputs = sext24(inputs);

            if iwt != 0 {
                self.dsp.mems[iwa as usize] = memval; // MEMVAL was selected in previous MRD
                if ira == iwa {
                    inputs = memval;
                }
            }

            // Operand sel
            let mut b: i32; // 26-bit
            if zero == 0 {
                if bsel != 0 {
                    b = acc;
                } else {
                    b = sext24(self.dsp.temp[((tra + self.dsp.dec) & 0x7f) as usize]);
                }
                if negb != 0 {
                    b = 0i32.wrapping_sub(b);
                }
            } else {
                b = 0;
            }

            let x: i32 = if xsel != 0 {
                inputs
            } else {
                sext24(self.dsp.temp[((tra + self.dsp.dec) & 0x7f) as usize])
            };

            let mut y: i32 = 0; // 13 bit
            if ysel == 0 {
                y = frc_reg;
            } else if ysel == 1 {
                y = (self.dsp.coef[coef] as i32) >> 3; // COEF is 16 bits
            } else if ysel == 2 {
                y = (y_reg >> 11) & 0x1fff;
            } else if ysel == 3 {
                y = (y_reg >> 4) & 0x0fff;
            }

            if yrl != 0 {
                y_reg = inputs;
            }

            // Shifter
            let shifted: i32 = if shift == 0 {
                acc.clamp(-0x00800000, 0x007fffff)
            } else if shift == 1 {
                acc.wrapping_mul(2).clamp(-0x00800000, 0x007fffff)
            } else if shift == 2 {
                sext24(acc.wrapping_mul(2))
            } else {
                sext24(acc)
            };

            // ACCUM
            y = sext13(y);

            let v = ((x as i64) * (y as i64)) >> 12;
            acc = (v + b as i64) as i32;

            if twt != 0 {
                self.dsp.temp[((twa + self.dsp.dec) & 0x7f) as usize] = shifted;
            }

            if frcl != 0 {
                if shift == 3 {
                    frc_reg = shifted & 0x0fff;
                } else {
                    frc_reg = (shifted >> 11) & 0x1fff;
                }
            }

            if mrd != 0 || mwt != 0 {
                let mut addr = self.dsp.madrs[masa] as u32;
                if table == 0 {
                    addr = addr.wrapping_add(self.dsp.dec);
                }
                if adreb != 0 {
                    addr += adrs_reg & 0x0FFF;
                }
                if nxadr != 0 {
                    addr += 1;
                }
                if table == 0 {
                    addr &= self.dsp.rbl - 1;
                } else {
                    addr &= 0xffff;
                }
                addr += self.dsp.rbp << 12;
                addr <<= 1;
                if mrd != 0 && (step & 1) != 0 {
                    // memory only allowed on odd? DoA inserts NOPs on even
                    if nofl != 0 {
                        memval = (self.read_word_ram(addr) as i32) << 8;
                    } else {
                        memval = dsp_unpack(self.read_word_ram(addr));
                    }
                }
                if mwt != 0 && (step & 1) != 0 {
                    if nofl != 0 {
                        self.write_word_ram(addr, (shifted >> 8) as u16);
                    } else {
                        self.write_word_ram(addr, dsp_pack(shifted));
                    }
                }
            }

            if adrl != 0 {
                if shift == 3 {
                    adrs_reg = ((shifted >> 12) & 0xfff) as u32;
                } else {
                    adrs_reg = (inputs >> 16) as u32;
                }
            }

            if ewt != 0 {
                self.dsp.efreg[ewa] = self.dsp.efreg[ewa].wrapping_add((shifted >> 8) as i16);
            }
        }
        self.dsp.dec = self.dsp.dec.wrapping_sub(1);
        self.dsp.mixs = [0; 16];
    }

    fn dsp_start(&mut self) {
        self.dsp.stopped = false;
        let mut i: i32 = 127;
        while i >= 0 {
            let base = i as usize * 4;
            if self.dsp.mpro[base] != 0
                || self.dsp.mpro[base + 1] != 0
                || self.dsp.mpro[base + 2] != 0
                || self.dsp.mpro[base + 3] != 0
            {
                break;
            }
            i -= 1;
        }
        self.dsp.last_step = (i + 1) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Word offsets used by the tests (68000 address = 0x100000 + offset*2):
    // slot 0 registers are word offsets 0x00-0x0f; common registers start at
    // byte 0x400 -> word offset 0x200.
    const REG_MVOL: u32 = 0x200; // byte 0x400
    const REG_MIDI_IN: u32 = 0x202; // byte 0x404
    const REG_TIMER_A: u32 = 0x20c; // byte 0x418
    const REG_SCIEB: u32 = 0x20f; // byte 0x41e
    const REG_SCIPD: u32 = 0x210; // byte 0x420
    const REG_SCIRE: u32 = 0x211; // byte 0x422
    const REG_SCILV0: u32 = 0x212; // byte 0x424

    /// Key on slot 0 with a 16-sample looping square wave at byte address
    /// 0x100, full volume, centered, played back at native rate.
    fn key_on_pcm16(scsp: &mut Scsp) {
        // 16 samples of a half-scale square wave, big-endian 16-bit
        for i in 0..16 {
            let v: i16 = if i < 8 { 0x4000 } else { -0x4000 };
            scsp.ram[0x100 + i * 2] = (v >> 8) as u8;
            scsp.ram[0x101 + i * 2] = (v & 0xff) as u8;
        }
        scsp.write(REG_MVOL, 0x000f, 0xffff); // MVOL = 15
        scsp.write(0x01, 0x0100, 0xffff); // SA = 0x100
        scsp.write(0x02, 0x0000, 0xffff); // LSA = 0
        scsp.write(0x03, 0x0010, 0xffff); // LEA = 16 samples
        scsp.write(0x04, 31, 0xffff); // AR = 31 (instant), D1R = D2R = 0 (sustain)
        scsp.write(0x05, 31, 0xffff); // KRS = 0, DL = 0, RR = 31 (fast release)
        scsp.write(0x06, 0x0000, 0xffff); // TL = 0
        scsp.write(0x08, 0x0000, 0xffff); // OCT = 0, FNS = 0 (native rate)
        scsp.write(0x0b, 7 << 13, 0xffff); // DISDL = 7 (0 dB), DIPAN = 0 (center)
                                           // KEYONEX | KEYONB | LPCTL = normal loop
        scsp.write(0x00, 0x1000 | 0x0800 | (1 << 5), 0xffff);
    }

    #[test]
    fn pcm16_slot_produces_sound() {
        let mut scsp = Scsp::new();
        key_on_pcm16(&mut scsp);
        let mut peak = 0i32;
        for _ in 0..4410 {
            let (l, r) = scsp.generate();
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak > 1000, "expected audible output, got peak {peak}");
    }

    #[test]
    fn key_off_decays_to_silence() {
        let mut scsp = Scsp::new();
        key_on_pcm16(&mut scsp);
        for _ in 0..100 {
            scsp.generate();
        }
        // KEYONEX with KEYONB clear -> release
        scsp.write(0x00, 0x1000 | (1 << 5), 0xffff);
        // RR = 31 releases in a few dozen samples; give it plenty of time
        for _ in 0..4410 {
            scsp.generate();
        }
        for _ in 0..1000 {
            let (l, r) = scsp.generate();
            assert_eq!((l, r), (0, 0), "expected silence after release");
        }
    }

    #[test]
    fn midi_in_raises_pending_and_irq_routing() {
        let mut scsp = Scsp::new();
        // route the MIDI interrupt (source SCIMID=3) through SCILV0 -> level 1
        scsp.write(REG_SCILV0, 1 << SCIMID, 0xffff);
        scsp.write(REG_SCIEB, 1 << SCIMID, 0xffff);
        assert!(scsp.irq_lines().iter().all(|&b| !b));

        scsp.midi_in(0x90);
        // MIDI input pending bit (SCIPD bit 3)
        assert!(scsp.read(REG_SCIPD) & (1 << SCIMID) != 0);
        let lines = scsp.irq_lines();
        assert!(lines[1], "expected irq level 1 asserted: {lines:?}");
        assert_eq!(lines.iter().filter(|&&b| b).count(), 1);

        // reading the MIDI data register pops the FIFO, clears the pending
        // bit and the irq
        assert_eq!(scsp.read(REG_MIDI_IN) & 0xff, 0x90);
        assert!(scsp.read(REG_SCIPD) & (1 << SCIMID) == 0);
        assert!(scsp.irq_lines().iter().all(|&b| !b));
    }

    #[test]
    fn timer_a_fires_and_raises_irq() {
        let mut scsp = Scsp::new();
        // route timer A (source SCITMA=6) through SCILV0 -> level 1
        scsp.write(REG_SCILV0, 1 << SCITMA, 0xffff);
        scsp.write(REG_SCIEB, 1 << SCITMA, 0xffff);
        // TimPris = 0 (-> 1), count = 0. The reference computes the period as
        // 512/((CLOCK/1)/255) = 512/88545 seconds, which at 44100 Hz lands
        // between samples 255 and 256, so the timer fires on the 256th sample.
        scsp.write(REG_TIMER_A, 0x0000, 0xffff);

        for _ in 0..255 {
            scsp.generate();
            assert!(scsp.read(REG_SCIPD) & 0x40 == 0, "timer fired early");
        }
        scsp.generate();
        assert!(scsp.read(REG_SCIPD) & 0x40 != 0, "timer did not fire");
        assert!(scsp.irq_lines()[1]);

        // acknowledge via SCIRE; TimCnt == 0xffff, so per real-hardware
        // behavior the pending bit immediately pops up again in SCIPD
        scsp.write(REG_SCIRE, 0x0040, 0xffff);
        assert!(scsp.read(REG_SCIPD) & 0x40 != 0);
    }
}
