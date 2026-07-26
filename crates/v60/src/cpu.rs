//! The V60 register file, reset, and fetch/dispatch harness.
//!
//!cpp`. Register indices follow the reference exactly so the
//! opcode handlers -- ported next -- read the same way: R0..R28 are general,
//! then AP, FP, SP, PC, PSW, and the privileged registers.

use crate::bus::Bus;

// Register-file indices.
pub const AP: usize = 29;
pub const FP: usize = 30;
pub const SP: usize = 31;
pub const PC: usize = 32;
pub const PSW: usize = 33;
pub const SBR: usize = 41;
pub const TR: usize = 42;
pub const SYCW: usize = 43;
pub const TKCW: usize = 44;
pub const PIR: usize = 45;
pub const PSW2: usize = 51;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct V60 {
    /// The whole register file. 52 named entries; sized to 64 for headroom.
    #[serde(with = "serde_big_array::BigArray")]
    pub reg: [u32; 64],
    // Condition flags, kept separate from PSW
    // the PSW is read.
    pub cy: bool,
    pub ov: bool,
    pub s: bool,
    pub z: bool,

    /// Address the current instruction started at.
    pub ppc: u32,
    pub icount: i32,

    // Addressing-mode decode scratch, shared by every operand read rather
    // than threaded through each one as arguments.
    pub modadd: u32,
    pub moddim: u8,
    pub modm: bool,
    pub modval: u8,
    pub modval2: u8,
    pub amout: u32,
    pub amflag: bool,
    /// Set when the decoded operand is an immediate literal (value already in
    /// `amout`) rather than a memory address or register.
    pub am_value: bool,
    /// Bit offset produced by the bit-address decoder: the operand is the byte base in `amout` plus this many
    /// bits. `bamoffset1`/`bamoffset2` hold the two operands' offsets for the
    /// F7b bit-string instructions.
    pub bamoffset: u32,
    pub bamoffset1: u32,
    pub bamoffset2: u32,

    // F12 two-operand format state.
    pub instflags: u8,
    pub op1: u32,
    pub flag1: bool,
    pub op2: u32,
    pub flag2: bool,
    pub amlength1: u32,
    pub amlength2: u32,

    // String/block-instruction state (the 0x58/0x5A groups): the sub-opcode
    // byte and the two block lengths held alongside it.
    pub subop: u8,
    pub lenop1: u32,
    pub lenop2: u32,

    /// Maskable interrupt request line. When asserted and PSW.IE is set, the CPU
    /// vectors through the SBR interrupt table before the next instruction.
    pub irq_line: bool,
    /// The vector the acknowledging device returns (the IRQ level on Model 1);
    /// the CPU adds 0x40 to index the interrupt table.
    pub irq_vector: u8,
    /// HALT stops instruction fetching until an enabled interrupt is accepted.
    pub halted: bool,
    /// Diagnostic: number of maskable interrupts actually accepted.
    pub irq_taken: u64,

    /// Optional instruction trace for PCs in `[trace_lo, trace_hi]`, up to
    /// `trace_cap` entries, deduping consecutive same-PC spins. Each entry is
    /// (pc, opcode). Off by default (`trace_cap = 0`).
    pub trace: Vec<(u32, u8)>,
    pub trace_lo: u32,
    pub trace_hi: u32,
    pub trace_cap: usize,

    /// Coverage: how many times each opcode byte was executed, and how many of
    /// those had no implementation yet. This is what makes "which opcode does
    /// the boot need next" a measured question.
    #[serde(skip, default = "default_hist256")]
    pub op_count: [u64; 256],
    #[serde(skip, default = "default_hist256")]
    pub op_unimpl: [u64; 256],
}

impl Default for V60 {
    fn default() -> Self {
        Self::new()
    }
}

impl V60 {
    pub fn new() -> Self {
        let mut cpu = Self {
            reg: [0; 64],
            cy: false,
            ov: false,
            s: false,
            z: false,
            ppc: 0,
            icount: 0,
            modadd: 0,
            moddim: 0,
            modm: false,
            modval: 0,
            modval2: 0,
            amout: 0,
            amflag: false,
            am_value: false,
            bamoffset: 0,
            bamoffset1: 0,
            bamoffset2: 0,
            instflags: 0,
            op1: 0,
            flag1: false,
            op2: 0,
            flag2: false,
            amlength1: 0,
            amlength2: 0,
            subop: 0,
            lenop1: 0,
            lenop2: 0,
            irq_line: false,
            irq_vector: 0,
            halted: false,
            irq_taken: 0,
            trace: Vec::new(),
            trace_lo: 0,
            trace_hi: 0,
            trace_cap: 0,
            op_count: [0; 256],
            op_unimpl: [0; 256],
        };
        cpu.reset();
        cpu
    }

    /// The reference. The reset PC is 0xfffffff0; on the 24-bit V60 bus
    /// that lands at 0xfffff0, where the boot ROM's first instruction (a JMP)
    /// sits.
    pub fn reset(&mut self) {
        self.reg = [0; 64];
        self.reg[PSW] = 0x1000_0000;
        self.reg[PC] = 0xffff_fff0;
        self.reg[SBR] = 0x0000_0000;
        self.reg[SYCW] = 0x0000_0070;
        self.reg[TKCW] = 0x0000_e000;
        self.reg[PSW2] = 0x0000_f002;
        self.cy = false;
        self.ov = false;
        self.s = false;
        self.z = false;
        self.icount = 0;
        self.irq_line = false;
        self.irq_vector = 0;
        self.halted = false;
    }

    #[inline]
    pub fn pc(&self) -> u32 {
        self.reg[PC]
    }

    /// Asserts the maskable IRQ line with the vector the device would return on
    /// acknowledge (the raw IRQ level; the CPU offsets it by 0x40). Level-held,
    /// like Model 1's GLUE: it stays pending until `clear_irq`.
    pub fn assert_irq(&mut self, vector: u8) {
        self.irq_line = true;
        self.irq_vector = vector;
    }
    /// Releases the IRQ line (the ISR does this via the GLUE irq-control port).
    pub fn clear_irq(&mut self) {
        self.irq_line = false;
    }

    /// Runs until the cycle budget is spent. Every instruction fetches its
    /// opcode byte and dispatches; unimplemented opcodes are counted and halt
    /// the slice so a boot trace stops exactly where the port must continue.
    pub fn run<B: Bus>(&mut self, bus: &mut B, cycles: i32) {
        self.icount += cycles;

        if self.irq_line {
            self.try_irq(bus);
        }
        if self.halted {
            self.icount = 0;
            return;
        }

        while self.icount > 0 {
            self.ppc = self.reg[PC];
            let op = bus.read_u8(self.reg[PC]);
            self.op_count[op as usize] += 1;
            let pcv = self.reg[PC] & 0x00ff_ffff;
            if self.trace.len() < self.trace_cap
                && pcv >= self.trace_lo
                && pcv <= self.trace_hi
                && self.trace.last().map(|e| e.0) != Some(pcv)
            {
                self.trace.push((pcv, op));
            }
            self.icount -= 8; // the reference uses a flat 8-cycle average

            match self.dispatch(bus, op) {
                Some(len) => self.reg[PC] = self.reg[PC].wrapping_add(len),
                None => {
                    self.op_unimpl[op as usize] += 1;
                    self.icount = 0;
                    return;
                }
            }

            // The reference generic FIFO asserts HALT through a zero-time
            // synchronization after accepting the overflow word. Complete
            // this instruction, discard its unused cycle budget, and stop
            // before fetching another instruction.
            if bus.halt_requested() {
                self.icount = 0;
                return;
            }

            if self.irq_line {
                self.try_irq(bus);
            }
            if self.halted {
                self.icount = 0;
                return;
            }
        }
    }

    /// Opcode dispatch. Returns the instruction length to advance PC by, or
    /// None when the opcode has no implementation yet. Handlers are filled in
    /// against the reference optable incrementally.
    fn dispatch<B: Bus>(&mut self, bus: &mut B, op: u8) -> Option<u32> {
        self.exec_op(bus, op)
    }
}

fn default_hist256() -> [u64; 256] {
    [0; 256]
}
