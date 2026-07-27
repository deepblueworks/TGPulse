//! ADSP-21062 internal state.

/// A general register: 32 bits, read as either an integer or an IEEE float.
///
pub type SharcReg = u32;

/// One data-address generator: index, modify, base and length registers, eight
/// of each. DAG1 drives the DM bus (i0-i7), DAG2 the PM bus (i8-i15).
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Dag {
    pub i: [u32; 8],
    pub m: [u32; 8],
    pub b: [u32; 8],
    pub l: [u32; 8],
}

/// One of the ten DMA channels' register block.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct DmaRegs {
    pub control: u32,
    pub int_index: u32,
    pub int_modifier: u32,
    pub int_count: u32,
    pub chain_ptr: u32,
    pub gen_purpose: u32,
    pub ext_index: u32,
    pub ext_modifier: u32,
    pub ext_count: u32,
}

/// Internal SRAM: two banks. Program memory is 48-bit (stored in u64), data
/// memory 32-bit. On the 2M part each bank is 0x8000 words; the SHARC maps them
/// at 0x20000. We size generously so any Model 2B upload fits.
pub const BLOCK_WORDS: usize = 0x20000;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sharc {
    // --- Register file (active + alternate bank) ---
    pub r: [SharcReg; 16],
    pub reg_alt: [SharcReg; 16],

    // --- Program counter / pipeline ---
    pub pc: u32,
    pub daddr: u32,
    pub faddr: u32,
    pub nfaddr: u32,

    // --- Multiplier result accumulators (80-bit, kept as u64 pair halves) ---
    pub mrf: u64,
    pub mrb: u64,

    // --- Stacks ---
    pub pcstack: [u32; 32],
    pub pcstkp: u32,
    pub pcstk: u32,
    pub lcstack: [u32; 6],
    pub lastack: [u32; 6],
    pub lstkp: u32,
    pub laddr_addr: u32,
    pub laddr_code: u32,
    pub laddr_loop_type: u32,
    pub curlcntr: u32,
    pub lcntr: u32,

    // --- Data address generators ---
    pub dag1: Dag,
    pub dag2: Dag,
    pub dag1_alt: Dag,
    pub dag2_alt: Dag,

    // --- System registers ---
    pub mode1: u32,
    pub mode2: u32,
    pub astat: u32,
    pub stky: u32,
    pub irptl: u32,
    pub imask: u32,
    pub imaskp: u32,
    pub ustat1: u32,
    pub ustat2: u32,
    pub flag: [u32; 4],
    pub syscon: u32,
    pub sysstat: u32,
    pub px: u64,

    // Status stack (mode1 + astat saved on interrupt / push sts).
    pub status_stack: [(u32, u32); 5],
    pub status_stkp: i32,

    // --- DMA ---
    pub dma: [DmaRegs; 12],
    pub dma_status: u32,

    // --- Interrupts / idle ---
    pub idle: bool,
    pub irq_pending: u32,
    pub active_irq_num: i32,
    /// True between vectoring to a handler and its RTI.
    pub interrupt_active: bool,

    // --- Microcode upload bookkeeping (external DMA, channel 6) ---
    pub extdma_shift: u8,
    /// Host-port write pairing state for `external_iop_write`.
    pub iop_write_num: u32,
    pub iop_data: u32,

    // --- Internal SRAM ---
    pub pm: Vec<u64>, // program memory, 48-bit words
    pub dm: Vec<u32>, // data memory, 32-bit words

    // Floating-point constants the compute unit uses.
    pub icount: i32,
    pub opcode: u64,

    /// Count of instructions whose handler is not yet ported, and the last such
    /// class, so the porting gaps are visible during bring-up.
    pub unimpl_count: u64,
    pub last_unimpl: crate::ops::Op,
    /// Per-class count of unported instruction classes, indexed by `Op as usize`.
    #[serde(skip, default = "default_unimpl_hist")]
    pub unimpl_hist: [u64; 64],
    /// Bring-up counters: instructions retired, and external DM reads/writes.
    pub insns: u64,
    pub ext_reads: u64,
    pub ext_writes: u64,
}

impl Default for Sharc {
    fn default() -> Self {
        Self::new()
    }
}

impl Sharc {
    pub fn new() -> Self {
        Sharc {
            r: [0; 16],
            reg_alt: [0; 16],
            pc: 0,
            daddr: 0,
            faddr: 0,
            nfaddr: 0,
            mrf: 0,
            mrb: 0,
            pcstack: [0; 32],
            pcstkp: 0,
            pcstk: 0,
            lcstack: [0; 6],
            lastack: [0; 6],
            lstkp: 0,
            laddr_addr: 0,
            laddr_code: 0,
            laddr_loop_type: 0,
            curlcntr: 0,
            lcntr: 0,
            dag1: Dag::default(),
            dag2: Dag::default(),
            dag1_alt: Dag::default(),
            dag2_alt: Dag::default(),
            mode1: 0,
            mode2: 0,
            astat: 0,
            stky: 0,
            irptl: 0,
            imask: 0,
            imaskp: 0,
            ustat1: 0,
            ustat2: 0,
            flag: [0; 4],
            syscon: 0,
            sysstat: 0,
            px: 0,
            status_stack: [(0, 0); 5],
            status_stkp: 0,
            dma: [DmaRegs::default(); 12],
            dma_status: 0,
            idle: false,
            irq_pending: 0,
            active_irq_num: -1,
            interrupt_active: false,
            extdma_shift: 0,
            iop_write_num: 0,
            iop_data: 0,
            pm: vec![0; BLOCK_WORDS],
            dm: vec![0; BLOCK_WORDS],
            icount: 0,
            opcode: 0,
            unimpl_count: 0,
            last_unimpl: crate::ops::Op::Nop,
            unimpl_hist: [0; 64],
            insns: 0,
            ext_reads: 0,
            ext_writes: 0,
        }
    }

    /// Resets to the Model 2 boot state: halted, host boot mode, DMA channel 6
    /// primed to receive the microcode the i960 uploads.
    pub fn reset(&mut self) {
        for w in self.pm.iter_mut() {
            *w = 0;
        }
        for w in self.dm.iter_mut() {
            *w = 0;
        }
        // BOOT_MODE_HOST: DMA6 waits for the host to feed the program.
        self.dma[6].int_index = 0x20000;
        self.dma[6].int_modifier = 1;
        self.dma[6].int_count = 0x100;
        self.dma[6].ext_index = 0x400000;
        self.dma[6].ext_modifier = 1;
        self.dma[6].ext_count = 0x600;
        self.dma[6].control = 0xa1;
        // Pipeline and stack state.
        self.pc = 0x20004;
        self.daddr = self.pc + 1;
        self.faddr = self.daddr + 1;
        self.nfaddr = self.faddr + 1;
        self.stky = crate::consts::PCEM | crate::consts::SSEM | crate::consts::LSEM;
        self.pcstkp = 0;
        self.lstkp = 0;
        self.pcstk = 0x00ff_ffff;
        self.curlcntr = 0xffff_ffff;
        self.laddr_addr = 0x00ff_ffff;
        self.laddr_code = 0x1f;
        self.laddr_loop_type = 0x3;
        self.status_stkp = 0;
        self.interrupt_active = false;
        self.irq_pending = 0;
        self.active_irq_num = -1;
        self.syscon = 0x0000_0010;
        self.iop_write_num = 0;
        self.iop_data = 0;
        self.extdma_shift = 0;
        self.idle = false;
    }
}

fn default_unimpl_hist() -> [u64; 64] {
    [0; 64]
}
