pub const I960_RCACHE_SIZE: usize = 4;

// Register Indices
pub const PFP: usize = 0;
pub const SP: usize = 1;
pub const RIP: usize = 2;
pub const FP: usize = 31;

// --- Fault Types ---
pub const FAULT_TRACE: u32 = 1;
pub const FAULT_OPERATION: u32 = 2;
pub const FAULT_ARITHMETIC: u32 = 3;
pub const FAULT_FLOATING_POINT: u32 = 4;
pub const FAULT_CONSTRAINT: u32 = 5;
pub const FAULT_PROTECTION: u32 = 7;
pub const FAULT_TYPE: u32 = 8;

// --- Fault Subtypes (Arithmetic) ---
pub const FSUB_ORDINAL_OVERFLOW: u32 = 1;
pub const FSUB_INTEGER_OVERFLOW: u32 = 2;
pub const FSUB_ZERO_DIVIDE: u32 = 3;

// Internal state for burst transfers
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StallState {
    pub t1: u32,
    pub t2: usize,
    pub index: usize,
    pub size: usize,
    pub burst_mode: bool,
    pub is_write_op: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct I960Cpu {
    // Registers r0-r15 (local), g0-g15 (global)
    pub r: [u32; 32],

    // Register Cache
    pub rcache: [[u32; 16]; I960_RCACHE_SIZE],
    pub rcache_frame_addr: [u32; I960_RCACHE_SIZE],
    pub rcache_pos: i32,

    // Floating point registers
    pub fp: [f64; 4],

    /// Distinct (major, sub) pairs of floating-point ops the dispatcher has no
    /// implementation for. A real part faults on these; we record them so a
    /// silently-stale destination register can be traced back here.
    pub fpu_unimpl: Vec<(u32, u32)>,

    // Special Function Registers
    pub sat: u32,
    pub prcb: u32,
    pub pc: u32,
    pub ac: u32,
    pub ip: u32,
    /// Optional ring buffer of executed IPs, for post-mortem tracing.
    pub trace: Option<(Vec<u32>, usize)>,
    /// Recording stops the first time the IP reaches this address, so the ring
    /// still holds the path *into* it rather than the loop that follows.
    pub trace_stop: u32,
    /// When non-zero, dumps registers each time the IP reaches this address.
    /// Which external line latched `immediate_vector`, so the request can be
    /// dropped if the board deasserts it before the CPU gets to it.
    pub immediate_line: Option<usize>,
    /// Debugger breakpoints. Execution stops *before* the instruction at a
    /// listed address, leaving the machine inspectable at that point.
    #[serde(skip)]
    pub breakpoints: Vec<u32>,
    #[serde(skip)]
    pub bp_hit: Option<u32>,
    pub trace_frozen: bool,
    pub pip: u32,
    pub icr: u32,

    // --- Timer Registers (Timer 0, Timer 1) ---
    pub tmr: [u32; 2], // Mode Register
    pub tcr: [u32; 2], // Count Register
    pub trr: [u32; 2], // Reload Register

    // Interrupt State
    pub immediate_irq: bool,
    pub immediate_vector: i32,
    pub immediate_pri: i32,

    // NEW: Deferral state for when set_irq_line is called without Bus access
    pub pending_irq_check: bool,
    pub deferred_vector: i32,
    /// Latched external IRQ input levels. The i960 queues an interrupt only
    /// on a low-to-high transition, not every time the board recomputes an
    /// already asserted line.
    pub irq_line_state: [bool; 4],
    /// Diagnostics for validating external interrupt delivery against real
    /// game code. These do not affect architectural state.
    pub interrupt_count: u64,
    pub last_interrupt_vector: i32,
    pub last_interrupt_handler: u32,

    // Execution State
    pub icount: i32,
    pub stalled: bool,

    // Burst Stall State
    pub stall_state: StallState,

    /// Runtime-only handle to the dynarec's compiled-block cache, type-erased
    /// because the cache is monomorphized over the bus type. JIT code is an
    /// execution detail, never architectural state: it is skipped on
    /// serialization, and cloning a CPU (savestates, dual-run checks) starts
    /// the copy with a cold cache.
    #[serde(skip)]
    pub jit: JitSlot,
}

/// Type-erased box for the compiled-block cache; see `I960Cpu::jit`.
#[derive(Default)]
pub struct JitSlot(pub Option<Box<dyn std::any::Any + Send>>);

impl Clone for JitSlot {
    fn clone(&self) -> Self {
        JitSlot(None)
    }
}

impl std::fmt::Debug for JitSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JitSlot(..)")
    }
}

impl Default for I960Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl I960Cpu {
    pub fn new() -> Self {
        Self {
            r: [0; 32],
            rcache: [[0; 16]; I960_RCACHE_SIZE],
            rcache_frame_addr: [0; I960_RCACHE_SIZE],
            rcache_pos: 0,
            fp: [0.0; 4],
            fpu_unimpl: Vec::new(),
            sat: 0,
            prcb: 0,
            pc: 0x001f2002,
            ac: 0,
            ip: 0,
            pip: 0,
            icr: 0,

            // Initialize Timers
            tmr: [0; 2],
            tcr: [0; 2],
            trr: [0; 2],

            immediate_irq: false,
            immediate_vector: 0,
            immediate_pri: 0,

            // Initialize Deferral State
            pending_irq_check: false,
            deferred_vector: 0,
            irq_line_state: [false; 4],
            interrupt_count: 0,
            last_interrupt_vector: -1,
            last_interrupt_handler: 0,

            icount: 0,
            stalled: false,

            trace: None,
            immediate_line: None,
            trace_stop: 0,
            breakpoints: Vec::new(),
            bp_hit: None,
            trace_frozen: false,
            stall_state: StallState::default(),
            jit: JitSlot::default(),
        }
    }
}
