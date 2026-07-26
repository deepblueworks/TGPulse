//! MB86235 internal state.

/// Status-word (ST) bits, from the published tables.
pub mod flag {
    pub const AD: u32 = 0x0000_0001; // ALU divide-by-zero
    pub const AU: u32 = 0x0000_0002; // ALU underflow
    pub const AV: u32 = 0x0000_0004; // ALU overflow
    pub const AZ: u32 = 0x0000_0008; // ALU zero
    pub const AN: u32 = 0x0000_0010; // ALU negative
    pub const ZC: u32 = 0x0000_0100; // zero count
    pub const IL: u32 = 0x0000_0200; // illegal
    pub const NR: u32 = 0x0000_0400; // not rounded
    pub const ZD: u32 = 0x0000_0800; // zero divide
    pub const RP: u32 = 0x0000_4000; // repeat active
    pub const LP: u32 = 0x0000_8000; // loop active
    pub const MD: u32 = 0x0001_0000; // multiplier divide-by-zero
    pub const MU: u32 = 0x0002_0000; // multiplier underflow
    pub const MV: u32 = 0x0004_0000; // multiplier overflow
    pub const MZ: u32 = 0x0008_0000; // multiplier zero
    pub const MN: u32 = 0x0010_0000; // multiplier negative
    pub const F0: u32 = 0x1000_0000;
    pub const F1: u32 = 0x2000_0000;
    pub const F2: u32 = 0x4000_0000;
}

/// Program RAM: 4096 64-bit instruction words.
pub const PROGRAM_WORDS: usize = 0x1000;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Mb86235 {
    // --- Program counter and its stack ---
    pub pc: u32,
    pub delay_pc: u32,
    pub ppc: u32,
    pub delay_slot: bool,
    pub pcs: [u32; 4],
    pub pcp: u32,

    // --- Register file ---
    /// Address registers, banks A and B.
    pub aa: [u32; 8],
    pub ab: [u32; 8],
    /// Multiplier registers, banks A and B.
    pub ma: [u32; 8],
    pub mb: [u32; 8],
    /// General address registers used by the transfer slots.
    pub ar: [u32; 8],

    // --- Control registers ---
    pub sp: u32,
    pub eb: u32,
    pub eo: u32,
    pub rpc: u32,
    pub lpc: u32,
    pub mod_: u32,
    pub st: u32,

    /// PR ring buffer with its read and write pointers.
    pub pr: [u32; 24],
    pub prp: u32,
    pub pwp: u32,

    // --- Data ports ---
    pub pdr: u32,
    pub ddr: u32,

    // --- Program memory ---
    pub program: Vec<u64>,
    /// Internal data RAM A and B, 1024 words each. The A bus reaches external
    /// memory above 0x400; the B bus is internal only.
    pub dataa: Vec<u32>,
    pub datab: Vec<u32>,
    /// Set when a FIFO access stalled this instruction, which freezes the
    /// address-generator post-increments so the access can be retried.
    pub stalled: bool,
    /// PC to resume from when an instruction stalled on the input FIFO.
    pub stall_pc: u32,

    pub icount: i32,

    /// Distinct opcode classes with no handler yet, and how many times each
    /// was reached. An illegal type is a fault on hardware; recording them keeps
    /// the porting gaps visible while the microcode is brought up.
    pub unimpl: [u64; 8],
    pub insns: u64,
}

impl Default for Mb86235 {
    fn default() -> Self {
        Self::new()
    }
}

impl Mb86235 {
    pub fn new() -> Self {
        Mb86235 {
            pc: 0,
            delay_pc: 0,
            ppc: 0,
            delay_slot: false,
            pcs: [0; 4],
            pcp: 0,
            aa: [0; 8],
            ab: [0; 8],
            ma: [0; 8],
            mb: [0; 8],
            ar: [0; 8],
            sp: 0,
            eb: 0,
            eo: 0,
            rpc: 0,
            lpc: 0,
            mod_: 0,
            st: 0,
            pr: [0; 24],
            prp: 0,
            pwp: 0,
            pdr: 0,
            ddr: 0,
            program: vec![0; PROGRAM_WORDS],
            dataa: vec![0; 0x400],
            datab: vec![0; 0x400],
            stalled: false,
            stall_pc: 0,
            icount: 0,
            unimpl: [0; 8],
            insns: 0,
        }
    }

    /// Resets the core. The Model 2C board releases it from halt after the
    /// i960 has finished uploading the microcode, so execution starts at 0.
    pub fn reset(&mut self) {
        self.pc = 0;
        self.delay_pc = 0;
        self.ppc = 0;
        self.delay_slot = false;
        self.pcs = [0; 4];
        self.pcp = 0;
        self.sp = 0;
        self.st = 0;
        self.mod_ = 0;
        self.prp = 0;
        self.pwp = 0;
    }

    /// Stores one 32-bit half of a program word during the host upload.
    pub fn upload_program_half(&mut self, index: u32, data: u32) {
        let slot = (index / 2) as usize;
        let Some(word) = self.program.get_mut(slot) else {
            return;
        };
        if index & 1 != 0 {
            *word = (*word & 0x0000_0000_ffff_ffff) | ((data as u64) << 32);
        } else {
            *word = (*word & 0xffff_ffff_0000_0000) | data as u64;
        }
    }
}
