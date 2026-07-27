//! Cranelift block dynarec for the i960, with the interpreter as an exact
//! per-instruction fallback.
//!
//! Structure, in the spirit of Gecko's PowerPC JIT:
//!
//! * A **block** is a basic block of guest code: compilation starts at the
//!   entry IP and stops at the first control-flow instruction (or after a
//!   fallback call that can redirect control flow, or at a 32-instruction
//!   cap). Keeping blocks at basic-block length means IRQ line changes are
//!   observed between blocks, the same latency the interpreter's
//!   per-instruction loop provides.
//! * Blocks are cached by entry address. All guest state lives in the
//!   `I960Cpu` struct in memory (reached through `offset_of!` offsets), so a
//!   block can call back into Rust at any point with the architected state
//!   already consistent. Only `icount` is kept in an SSA value across the
//!   block, synced at every exit and around fallback calls.
//! * Every bus access goes through a trampoline back into Rust -- the board's
//!   MMIO reads and writes have side effects (FIFO pops, timer snapshots,
//!   IRQ acks) that must happen in program order. The trampoline also folds
//!   in the `take_stall` poll the interpreter runs after each instruction,
//!   setting `cpu.stalled`; the block checks that flag after each access and
//!   rewinds `ip = pip` on exit, exactly like the interpreter's `break`.
//! * Any opcode not lowered natively is emitted as a call to
//!   `I960Cpu::jit_step`, which runs the interpreter's per-instruction path
//!   verbatim. "Not lowered" therefore never means "behaves differently".
//! * Self-modifying/uploaded code: the board bumps a per-page epoch
//!   (`Bus::code_epoch`) on writes to executable RAM; a cached block records
//!   the epochs of the pages it spans and is recompiled when one moves.
//!
//! Cycle accounting matches the interpreter exactly: each lowered instruction
//! publishes `LIVE_ICOUNT` with the pre-decrement count, decrements by the
//! same constant the interpreter uses, and runs the internal-timer tick
//! (through a Rust helper, gated on a timer actually being enabled) after the
//! instruction. Mid-block stall exits leave `icount` decremented and `ip`
//! rewound, as the interpreter does.
//!
//! Debug aids that need per-instruction visibility -- breakpoints and the
//! trace ring -- force the interpreter path for as long as they are active.

// cranelift 0.134 renamed the `_imm` builders to `_imm_s`/`_imm_u`; the old
// names keep their sign-extending behaviour, which is exactly what the
// lowering wants (negative icount charges, negative displacements).
#![allow(deprecated)]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, AbiParam, FuncRef, InstBuilder, MemFlagsData, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use crate::bus::Bus;
use crate::cpu::core::LIVE_ICOUNT;
use crate::cpu::defs::I960Cpu;

pub mod dualrun;

/// Instructions per block cap. Basic blocks are far shorter than this; the
/// cap only bounds pathological straight-line runs so IRQ sampling stays
/// timely.
const MAX_BLOCK_INSNS: usize = 32;

/// Dev-only override for the block length cap, used by the dual-run checker:
/// single-instruction blocks make the JIT stop at exactly the cycle counts
/// the interpreter does, so the two can be diffed in lockstep. 0 = no
/// override.
pub static BLOCK_CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// Compiled blocks are entered with the CPU and a type-erased bus, and return
// an exit code: 0 = block ran to its end, 1 = stalled (IP already rewound).
type BlockFn = unsafe extern "C" fn(*mut I960Cpu, *mut c_void) -> i32;

// Field offsets into `I960Cpu`, computed at build time so the layout is free
// to change above us.
const OFF_R: i32 = offset_of!(I960Cpu, r) as i32;
const OFF_AC: i32 = offset_of!(I960Cpu, ac) as i32;
const OFF_IP: i32 = offset_of!(I960Cpu, ip) as i32;
const OFF_PIP: i32 = offset_of!(I960Cpu, pip) as i32;
const OFF_ICOUNT: i32 = offset_of!(I960Cpu, icount) as i32;
const OFF_TMR: i32 = offset_of!(I960Cpu, tmr) as i32;
const OFF_STALLED: i32 = offset_of!(I960Cpu, stalled) as i32;

// --- Rust callbacks reachable from compiled code -----------------------------

// Every bus access funnels through one of these. Each one also performs the
// interpreter's post-instruction `take_stall` poll; doing it per access
// rather than per instruction is indistinguishable, because the flag can
// only have been set by an access earlier in the same instruction.
unsafe extern "C" fn t_read_u32<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32) -> u32 {
    let bus = unsafe { &mut *(bus as *mut B) };
    let v = bus.read_u32(addr);
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
    v
}

unsafe extern "C" fn t_read_u16<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32) -> u32 {
    let bus = unsafe { &mut *(bus as *mut B) };
    let v = bus.read_u16(addr) as u32;
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
    v
}

unsafe extern "C" fn t_read_byte<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32) -> u32 {
    let bus = unsafe { &mut *(bus as *mut B) };
    let v = bus.read_byte(addr) as u32;
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
    v
}

unsafe extern "C" fn t_write_u32<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32, val: u32) {
    let bus = unsafe { &mut *(bus as *mut B) };
    bus.write_u32(addr, val);
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
}

unsafe extern "C" fn t_write_u16<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32, val: u32) {
    let bus = unsafe { &mut *(bus as *mut B) };
    bus.write_u16(addr, val as u16);
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
}

unsafe extern "C" fn t_write_byte<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32, val: u32) {
    let bus = unsafe { &mut *(bus as *mut B) };
    bus.write_byte(addr, val as u8);
    if bus.take_stall() {
        unsafe { (*cpu).stalled = true };
    }
}

/// Fallback for every opcode not lowered natively: the interpreter's exact
/// per-instruction path. Returns its status (1 = stalled).
unsafe extern "C" fn t_fallback<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, addr: u32) -> i32 {
    unsafe { (*cpu).jit_step(&mut *(bus as *mut B), addr) }
}

/// Internal-timer tick, called only when a timer is actually enabled (the
/// compiled code checks the enable bits inline first).
unsafe extern "C" fn t_timers<B: Bus>(cpu: *mut I960Cpu, bus: *mut c_void, elapsed: u32) {
    unsafe { (*cpu).tick_timers(&mut *(bus as *mut B), elapsed) }
}

struct HelperIds {
    read_u32: FuncId,
    read_u16: FuncId,
    read_byte: FuncId,
    write_u32: FuncId,
    write_u16: FuncId,
    write_byte: FuncId,
    fallback: FuncId,
    timers: FuncId,
}

#[derive(Clone, Copy)]
struct CompiledBlock {
    f: BlockFn,
    /// Entry address this block was compiled for (direct-mapped cache tag).
    tag: u32,
    // Pages (addr >> 12) the block spans, and their code epochs at compile
    // time. A write to either page recompiles the block on next lookup.
    p0: u32,
    p1: u32,
    e0: u64,
    e1: u64,
}

/// Block cache size (direct-mapped L1, indexed by `ip >> 2`) over a HashMap
/// backing store. Games touch tens of thousands of distinct block entries
/// over an attract loop, so a purely direct-mapped cache thrashes: every
/// collision costs a recompile. The L1 absorbs the hot loop lookups; anything
/// evicted from it is still found in the map instead of being recompiled.
const CACHE_BITS: u32 = 13;
const CACHE_SIZE: usize = 1 << CACHE_BITS;

pub struct Jit<B: Bus> {
    module: JITModule,
    helpers: HelperIds,
    block_sig: ir::Signature,
    fb_ctx: FunctionBuilderContext,
    l1: Vec<Option<CompiledBlock>>,
    cache: std::collections::HashMap<u32, CompiledBlock>,
    counter: u64,
    /// Diagnostics: blocks compiled, and recompiles caused by code writes.
    pub compiles: u64,
    pub recompiles: u64,
    _pd: PhantomData<fn(B)>,
}

impl<B: Bus> Jit<B> {
    pub fn new() -> Box<Self> {
        let mut flags = settings::builder();
        flags.set("opt_level", "none").expect("opt_level");
        // The verifier re-checks every block's IR and costs more than the
        // rest of the pipeline at this block size; the lowering is exercised
        // by the test suite (dual-run included) instead.
        flags
            .set("enable_verifier", "false")
            .expect("enable_verifier");
        let isa = cranelift_native::builder()
            .expect("host ISA")
            .finish(settings::Flags::new(flags))
            .expect("ISA");
        let mut builder =
            JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("i960_read_u32", t_read_u32::<B> as *const u8);
        builder.symbol("i960_read_u16", t_read_u16::<B> as *const u8);
        builder.symbol("i960_read_byte", t_read_byte::<B> as *const u8);
        builder.symbol("i960_write_u32", t_write_u32::<B> as *const u8);
        builder.symbol("i960_write_u16", t_write_u16::<B> as *const u8);
        builder.symbol("i960_write_byte", t_write_byte::<B> as *const u8);
        builder.symbol("i960_fallback", t_fallback::<B> as *const u8);
        builder.symbol("i960_timers", t_timers::<B> as *const u8);
        let mut module = JITModule::new(builder);

        let ptr = module.target_config().pointer_type();
        let sig = |params: &[ir::Type], rets: &[ir::Type], module: &mut JITModule| {
            let mut s = module.make_signature();
            for p in params {
                s.params.push(AbiParam::new(*p));
            }
            for r in rets {
                s.returns.push(AbiParam::new(*r));
            }
            s
        };
        let io = sig(&[ptr, ptr, types::I32], &[types::I32], &mut module);
        let wo = sig(&[ptr, ptr, types::I32, types::I32], &[], &mut module);
        let fb = sig(&[ptr, ptr, types::I32], &[types::I32], &mut module);
        let tm = sig(&[ptr, ptr, types::I32], &[], &mut module);
        let block_sig = sig(&[ptr, ptr], &[types::I32], &mut module);

        let helpers = HelperIds {
            read_u32: module
                .declare_function("i960_read_u32", Linkage::Import, &io)
                .expect("declare"),
            read_u16: module
                .declare_function("i960_read_u16", Linkage::Import, &io)
                .expect("declare"),
            read_byte: module
                .declare_function("i960_read_byte", Linkage::Import, &io)
                .expect("declare"),
            write_u32: module
                .declare_function("i960_write_u32", Linkage::Import, &wo)
                .expect("declare"),
            write_u16: module
                .declare_function("i960_write_u16", Linkage::Import, &wo)
                .expect("declare"),
            write_byte: module
                .declare_function("i960_write_byte", Linkage::Import, &wo)
                .expect("declare"),
            fallback: module
                .declare_function("i960_fallback", Linkage::Import, &fb)
                .expect("declare"),
            timers: module
                .declare_function("i960_timers", Linkage::Import, &tm)
                .expect("declare"),
        };

        Box::new(Self {
            module,
            helpers,
            block_sig,
            fb_ctx: FunctionBuilderContext::new(),
            l1: vec![None; CACHE_SIZE],
            cache: std::collections::HashMap::new(),
            counter: 0,
            compiles: 0,
            recompiles: 0,
            _pd: PhantomData,
        })
    }

    /// The block for `ip`, compiling (or recompiling after a code write) as
    /// needed.
    fn block_for(&mut self, bus: &mut B, ip: u32) -> BlockFn {
        let slot = ((ip >> 2) as usize) & (CACHE_SIZE - 1);
        if let Some(b) = self.l1[slot] {
            if b.tag == ip {
                if bus.code_epoch(b.p0) == b.e0
                    && (b.p1 == b.p0 || bus.code_epoch(b.p1) == b.e1)
                {
                    return b.f;
                }
            }
        }
        match self.cache.get(&ip) {
            Some(b)
                if bus.code_epoch(b.p0) == b.e0
                    && (b.p1 == b.p0 || bus.code_epoch(b.p1) == b.e1) =>
            {
                let b = *b;
                self.l1[slot] = Some(b);
                b.f
            }
            _ => {
                // Either never compiled, or the page it was compiled from has
                // been written since -- uploaded or self-modified code.
                // Recompile against the current bytes. (The old code memory
                // is not reclaimed; cranelift-jit cannot free individual
                // functions, and uploads are infrequent.)
                if self.cache.contains_key(&ip) {
                    self.recompiles += 1;
                }
                self.compiles += 1;
                let b = self.compile(bus, ip);
                let f = b.f;
                self.cache.insert(ip, b);
                self.l1[slot] = Some(b);
                f
            }
        }
    }

    fn compile(&mut self, bus: &mut B, entry: u32) -> CompiledBlock {
        self.counter += 1;
        let name = format!("i960_blk_{entry:08x}_{}", self.counter);
        let fid = self
            .module
            .declare_function(&name, Linkage::Export, &self.block_sig)
            .expect("declare block");

        let Self {
            module,
            fb_ctx,
            helpers,
            block_sig,
            ..
        } = self;
        let frontend_cfg = module.target_config();
        let mut ctx = module.make_context();
        ctx.func.signature = block_sig.clone();

        let mut last_addr;
        {
            let mut fb = FunctionBuilder::new(&mut ctx.func, fb_ctx);
            let fr = FuncRefs {
                read_u32: module.declare_func_in_func(helpers.read_u32, fb.func),
                read_u16: module.declare_func_in_func(helpers.read_u16, fb.func),
                read_byte: module.declare_func_in_func(helpers.read_byte, fb.func),
                write_u32: module.declare_func_in_func(helpers.write_u32, fb.func),
                write_u16: module.declare_func_in_func(helpers.write_u16, fb.func),
                write_byte: module.declare_func_in_func(helpers.write_byte, fb.func),
                fallback: module.declare_func_in_func(helpers.fallback, fb.func),
                timers: module.declare_func_in_func(helpers.timers, fb.func),
            };

            let entry_bb = fb.create_block();
            fb.append_block_params_for_function_params(entry_bb);
            fb.switch_to_block(entry_bb);
            fb.seal_block(entry_bb);
            let cpu = fb.block_params(entry_bb)[0];
            let busp = fb.block_params(entry_bb)[1];

            let mut e = Emit {
                fb,
                fr: &fr,
                cpu,
                busp,
                live: Value::from_u32(0),
                icount: Value::from_u32(0),
                elapsed: Value::from_u32(0),
            };
            e.live = e
                .fb
                .ins()
                .iconst(types::I64, &LIVE_ICOUNT as *const _ as i64);
            // A stale stall flag from the previous block must not leak in;
            // the interpreter clears it per instruction.
            let zero8 = e.fb.ins().iconst(types::I8, 0);
            e.fb.ins().store(flags(), zero8, cpu, OFF_STALLED);
            e.icount = e.fb.ins().load(types::I32, flags(), cpu, OFF_ICOUNT);
            e.elapsed = e.iconst32(0);

            let cap = match BLOCK_CAP.load(std::sync::atomic::Ordering::Relaxed) {
                0 => MAX_BLOCK_INSNS,
                n => n,
            };
            let mut addr = entry;
            let mut n = 0;
            loop {
                let op = bus.read_u32(addr);
                last_addr = addr;
                match e.lower(addr, op) {
                    Flow::Next(next) => {
                        n += 1;
                        if n >= cap {
                            e.exit_to(next, addr);
                            break;
                        }
                        addr = next;
                    }
                    Flow::End => break,
                }
            }
            e.fb.finalize(frontend_cfg);
        }

        module
            .define_function(fid, &mut ctx)
            .expect("i960 jit: define_function");
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize");
        let ptr = module.get_finalized_function(fid);

        let p0 = entry >> 12;
        let p1 = last_addr.wrapping_add(8) >> 12;
        CompiledBlock {
            f: unsafe { std::mem::transmute::<*const u8, BlockFn>(ptr) },
            tag: entry,
            p0,
            p1,
            e0: bus.code_epoch(p0),
            e1: if p1 == p0 { 0 } else { bus.code_epoch(p1) },
        }
    }
}

struct FuncRefs {
    read_u32: FuncRef,
    read_u16: FuncRef,
    read_byte: FuncRef,
    write_u32: FuncRef,
    write_u16: FuncRef,
    write_byte: FuncRef,
    fallback: FuncRef,
    timers: FuncRef,
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// What the lowering of one instruction decided about the block's shape.
enum Flow {
    /// Straight-line; execution continues at this address.
    Next(u32),
    /// The instruction ended the block (control flow, or a fallback that may
    /// redirect it). The exit code is already emitted.
    End,
}

/// The block emitter. All guest state is accessed through the `I960Cpu`
/// struct in memory; `icount` alone is cached in an SSA value and synced at
/// exits and around fallback calls.
struct Emit<'a, 'b> {
    fb: FunctionBuilder<'a>,
    fr: &'b FuncRefs,
    cpu: Value,
    busp: Value,
    live: Value,
    icount: Value,
    /// Cycles spent in this block since the last timer tick. The interpreter
    /// ticks the internal timers after every instruction; the JIT
    /// accumulates and ticks once per block exit (or just before a fallback
    /// call, which ticks its own instruction internally), so a timer IRQ is
    /// observed with block latency -- the same bound IRQ line sampling has.
    elapsed: Value,
}

impl Emit<'_, '_> {
    fn iconst32(&mut self, v: u32) -> Value {
        self.fb.ins().iconst(types::I32, v as i32 as i64)
    }

    fn load_r(&mut self, idx: u32) -> Value {
        self.fb
            .ins()
            .load(types::I32, flags(), self.cpu, OFF_R + 4 * idx as i32)
    }

    fn store_r(&mut self, idx: u32, v: Value) {
        self.fb
            .ins()
            .store(flags(), v, self.cpu, OFF_R + 4 * idx as i32);
    }

    fn load_ac(&mut self) -> Value {
        self.fb.ins().load(types::I32, flags(), self.cpu, OFF_AC)
    }

    fn store_ac(&mut self, v: Value) {
        self.fb.ins().store(flags(), v, self.cpu, OFF_AC);
    }

    fn store_ip(&mut self, v: Value) {
        self.fb.ins().store(flags(), v, self.cpu, OFF_IP);
    }

    /// REG-format src1: bits 4-0 register, or a 5-bit literal when bit 11 is
    /// set.
    fn src1(&mut self, op: u32) -> Value {
        if op & 0x800 != 0 {
            self.iconst32(op & 0x1f)
        } else {
            self.load_r(op & 0x1f)
        }
    }

    /// REG-format src2: bits 18-14 register, or a 5-bit literal when bit 12
    /// is set.
    fn src2(&mut self, op: u32) -> Value {
        if op & 0x1000 != 0 {
            self.iconst32((op >> 14) & 0x1f)
        } else {
            self.load_r((op >> 14) & 0x1f)
        }
    }

    /// COBR src1: bits 23-19 register, or literal when bit 13 is set.
    fn csrc1(&mut self, op: u32) -> Value {
        if op & 0x2000 != 0 {
            self.iconst32((op >> 19) & 0x1f)
        } else {
            self.load_r((op >> 19) & 0x1f)
        }
    }

    fn csrc2(&mut self, op: u32) -> Value {
        self.load_r((op >> 14) & 0x1f)
    }

    /// Per-instruction prologue, mirroring the interpreter loop: publish the
    /// pre-decrement cycle count for mid-quantum timer reads, then charge the
    /// instruction's cost.
    fn prologue(&mut self, cost: i32) {
        self.fb.ins().store(flags(), self.icount, self.live, 0);
        if cost != 0 {
            self.icount = self.fb.ins().iadd_imm(self.icount, -(cost as i64));
            self.elapsed = self.fb.ins().iadd_imm(self.elapsed, cost as i64);
        }
    }

    /// Ticks the internal timers by the cycles accumulated since the last
    /// tick, then resets the accumulator. The tick touches the bus (a timer
    /// firing queues an interrupt through the PRCB), so it is a Rust call --
    /// but only when a timer is actually enabled, which compiled code checks
    /// inline.
    fn tick_timers_total(&mut self) {
        let t0 = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_TMR);
        let t1 = self
            .fb
            .ins()
            .load(types::I32, flags(), self.cpu, OFF_TMR + 4);
        let either = self.fb.ins().bor(t0, t1);
        let enabled = self.fb.ins().band_imm(either, 2);
        let cond = self.fb.ins().icmp_imm(IntCC::NotEqual, enabled, 0);
        let call_bb = self.fb.create_block();
        let cont_bb = self.fb.create_block();
        self.fb.ins().brif(cond, call_bb, &[], cont_bb, &[]);
        self.fb.switch_to_block(call_bb);
        self.fb.ins().call(self.fr.timers, &[self.cpu, self.busp, self.elapsed]);
        self.fb.ins().jump(cont_bb, &[]);
        self.fb.seal_block(call_bb);
        self.fb.switch_to_block(cont_bb);
        self.fb.seal_block(cont_bb);
        self.elapsed = self.iconst32(0);
    }

    /// After any bus access: when the board signalled an external wait,
    /// rewind the IP to the faulting instruction and leave the quantum --
    /// the interpreter's `self.ip = self.pip; break`. `pip` is stored by the
    /// instruction before its first access.
    fn stall_check(&mut self) {
        let s = self.fb.ins().load(types::I8, flags(), self.cpu, OFF_STALLED);
        let cond = self.fb.ins().icmp_imm(IntCC::NotEqual, s, 0);
        let abort_bb = self.fb.create_block();
        let cont_bb = self.fb.create_block();
        self.fb.ins().brif(cond, abort_bb, &[], cont_bb, &[]);
        self.fb.switch_to_block(abort_bb);
        let pip = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_PIP);
        self.store_ip(pip);
        self.fb.ins().store(flags(), self.icount, self.cpu, OFF_ICOUNT);
        let one = self.iconst32(1);
        self.fb.ins().return_(&[one]);
        self.fb.seal_block(abort_bb);
        self.fb.switch_to_block(cont_bb);
        self.fb.seal_block(cont_bb);
    }

    fn store_pip(&mut self, addr: u32) {
        let a = self.iconst32(addr);
        self.fb.ins().store(flags(), a, self.cpu, OFF_PIP);
    }

    fn call_read(&mut self, f: FuncRef, ea: Value) -> Value {
        let call = self.fb.ins().call(f, &[self.cpu, self.busp, ea]);
        self.fb.inst_results(call)[0]
    }

    fn call_write(&mut self, f: FuncRef, ea: Value, v: Value) {
        self.fb.ins().call(f, &[self.cpu, self.busp, ea, v]);
    }

    /// Effective address for the MEM formats, returning the EA value and the
    /// address of the following instruction (MEMB displacement words advance
    /// the IP past the instruction). The displacement is fetched through the
    /// bus at run time, like the interpreter does.
    fn ea(&mut self, addr: u32, op: u32) -> (Value, u32) {
        let abase = (op >> 14) & 0x1f;
        if op & 0x1000 == 0 {
            // MEMA
            let offset = op & 0x1fff;
            let ea = if op & 0x2000 != 0 {
                let b = self.load_r(abase);
                self.fb.ins().iadd_imm(b, offset as i64)
            } else {
                self.iconst32(offset)
            };
            return (ea, addr.wrapping_add(4));
        }
        // MEMB
        let index = op & 0x1f;
        let scale = ((op >> 7) & 7) as i64;
        let mode = (op >> 10) & 0xf;
        let scaled_index = |e: &mut Self| {
            let i = e.load_r(index);
            if scale == 0 {
                i
            } else {
                e.fb.ins().ishl_imm(i, scale)
            }
        };
        // Modes 5 and 0xC-0xF carry a 32-bit displacement word after the
        // instruction; the fetch is a bus read and can stall.
        let disp = |e: &mut Self| {
            let a = e.iconst32(addr.wrapping_add(4));
            let d = e.call_read(e.fr.read_u32, a);
            e.stall_check();
            d
        };
        match mode {
            0x4 => (self.load_r(abase), addr.wrapping_add(4)),
            0x5 => {
                let d = disp(self);
                // IP-relative: the base is the address *after* the disp word.
                let ea = self.fb.ins().iadd_imm(d, addr.wrapping_add(8) as i64);
                (ea, addr.wrapping_add(8))
            }
            0x6 => (scaled_index(self), addr.wrapping_add(4)),
            0x7 => {
                let b = self.load_r(abase);
                let i = scaled_index(self);
                (self.fb.ins().iadd(b, i), addr.wrapping_add(4))
            }
            0xc => (disp(self), addr.wrapping_add(8)),
            0xd => {
                let d = disp(self);
                let b = self.load_r(abase);
                (self.fb.ins().iadd(d, b), addr.wrapping_add(8))
            }
            0xe => {
                let d = disp(self);
                let i = scaled_index(self);
                (self.fb.ins().iadd(d, i), addr.wrapping_add(8))
            }
            0xf => {
                let d = disp(self);
                let b = self.load_r(abase);
                let db = self.fb.ins().iadd(d, b);
                let i = scaled_index(self);
                (self.fb.ins().iadd(db, i), addr.wrapping_add(8))
            }
            // The interpreter panics on the remaining modes; route through the
            // fallback instead of lowering them.
            _ => unreachable!("MEMB mode {mode:x} is never lowered"),
        }
    }

    /// Normal block exit at a compile-time-known next IP. `last` is the
    /// address of the block's final instruction, recorded as `pip` so the
    /// residual state matches the interpreter's at block boundaries.
    fn exit_to(&mut self, next: u32, last: u32) {
        self.tick_timers_total();
        self.store_pip(last);
        let ip = self.iconst32(next);
        self.store_ip(ip);
        self.fb.ins().store(flags(), self.icount, self.cpu, OFF_ICOUNT);
        let zero = self.iconst32(0);
        self.fb.ins().return_(&[zero]);
    }

    /// Block exit with a computed next IP (branch/select result).
    fn exit_with(&mut self, ip: Value, last: u32) {
        self.tick_timers_total();
        self.store_pip(last);
        self.store_ip(ip);
        self.fb.ins().store(flags(), self.icount, self.cpu, OFF_ICOUNT);
        let zero = self.iconst32(0);
        self.fb.ins().return_(&[zero]);
    }

    /// Emits a fallback call for `addr` and the status check around it. The
    /// fallback runs the interpreter's per-instruction path, so `icount` must
    /// be in memory before the call and is reloaded after.
    fn fallback(&mut self, addr: u32) -> Value {
        self.tick_timers_total();
        self.fb.ins().store(flags(), self.icount, self.live, 0);
        self.fb.ins().store(flags(), self.icount, self.cpu, OFF_ICOUNT);
        let a = self.iconst32(addr);
        let call = self
            .fb
            .ins()
            .call(self.fr.fallback, &[self.cpu, self.busp, a]);
        let status = self.fb.inst_results(call)[0];
        self.icount = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_ICOUNT);
        status
    }

    /// Fallback that cannot redirect control flow (FPU ops): the block
    /// continues unless the instruction stalled.
    fn fallback_continue(&mut self, addr: u32) -> Flow {
        let status = self.fallback(addr);
        let cond = self.fb.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let abort_bb = self.fb.create_block();
        let cont_bb = self.fb.create_block();
        self.fb.ins().brif(cond, abort_bb, &[], cont_bb, &[]);
        self.fb.switch_to_block(abort_bb);
        // jit_step already rewound ip and left icount in memory.
        self.fb.ins().return_(&[status]);
        self.fb.seal_block(abort_bb);
        self.fb.switch_to_block(cont_bb);
        self.fb.seal_block(cont_bb);
        Flow::Next(addr.wrapping_add(4))
    }

    /// Fallback that may redirect control flow: ends the block either way.
    fn fallback_end(&mut self, addr: u32) -> Flow {
        let status = self.fallback(addr);
        self.fb.ins().return_(&[status]);
        Flow::End
    }

    /// Unsigned/signed compare into AC bits 0-2 (4 = less, 2 = equal,
    /// 1 = greater).
    fn cmp_cc(&mut self, signed: bool, t1: Value, t2: Value) -> Value {
        let lt = if signed {
            self.fb.ins().icmp(IntCC::SignedLessThan, t1, t2)
        } else {
            self.fb.ins().icmp(IntCC::UnsignedLessThan, t1, t2)
        };
        let eq = self.fb.ins().icmp(IntCC::Equal, t1, t2);
        let four = self.iconst32(4);
        let two = self.iconst32(2);
        let one = self.iconst32(1);
        let ge = self.fb.ins().select(eq, two, one);
        self.fb.ins().select(lt, four, ge)
    }

    fn set_ac_cc(&mut self, cc: Value) {
        let ac = self.load_ac();
        let cleared = self.fb.ins().band_imm(ac, 0xFFFF_FFF8);
        let v = self.fb.ins().bor(cleared, cc);
        self.store_ac(v);
    }

    /// One instruction. `addr` is its address, `op` the already-fetched word.
    fn lower(&mut self, addr: u32, op: u32) -> Flow {
        let op_idx = op >> 24;
        let sub = (op >> 7) & 0xf;
        let dst = (op >> 19) & 0x1f;

        // CTRL-format 24-bit displacement, relative to addr+4.
        let disp24 = |op: u32| {
            let v = op & 0x00FF_FFFF;
            let s = if v & 0x0080_0000 != 0 { v | 0xFF00_0000 } else { v };
            s.wrapping_sub(4)
        };
        // COBR 13-bit displacement, relative to addr+4.
        let disp13 = |op: u32| {
            let v = op & 0x1FFF;
            let s = if v & 0x1000 != 0 { v | 0xFFFF_E000 } else { v };
            s.wrapping_sub(4)
        };

        match op_idx {
            // --- Control flow (all block-enders) ---
            0x08 => {
                // b
                self.prologue(1);
                let t = addr.wrapping_add(4).wrapping_add(disp24(op));
                let ip = self.iconst32(t);
                self.exit_with(ip, addr);
                Flow::End
            }
            0x0b => {
                // bal
                self.prologue(5);
                let ret = self.iconst32(addr.wrapping_add(4));
                self.store_r(30, ret);
                let t = addr.wrapping_add(4).wrapping_add(disp24(op));
                let ip = self.iconst32(t);
                self.exit_with(ip, addr);
                Flow::End
            }
            0x10..=0x17 => {
                // b<cc>
                self.prologue(1);
                let ac = self.load_ac();
                let cond = if op_idx == 0x10 {
                    let m = self.fb.ins().band_imm(ac, 7);
                    self.fb.ins().icmp_imm(IntCC::Equal, m, 0)
                } else {
                    let m = self.fb.ins().band_imm(ac, (op_idx - 0x10) as i64);
                    self.fb.ins().icmp_imm(IntCC::NotEqual, m, 0)
                };
                let taken = addr.wrapping_add(4).wrapping_add(disp24(op)) & !3;
                let fall = addr.wrapping_add(4);
                let t = self.iconst32(taken);
                let f = self.iconst32(fall);
                let ip = self.fb.ins().select(cond, t, f);
                self.exit_with(ip, addr);
                Flow::End
            }
            0x30 | 0x37 => {
                // bbc / bbs
                self.prologue(4);
                let bit = self.csrc1(op);
                let bit = self.fb.ins().band_imm(bit, 0x1f);
                let src = self.csrc2(op);
                let one = self.iconst32(1);
                let mask = self.fb.ins().ishl(one, bit);
                let set = self.fb.ins().band(src, mask);
                let is_set = self.fb.ins().icmp_imm(IntCC::NotEqual, set, 0);
                // bbc branches when the bit is clear, bbs when set; a taken
                // branch records "equal" in AC, a missed one clears the cc.
                let cond = if op_idx == 0x37 {
                    is_set
                } else {
                    let z = self.fb.ins().icmp_imm(IntCC::Equal, set, 0);
                    z
                };
                let two = self.iconst32(2);
                let zero = self.iconst32(0);
                let cc = self.fb.ins().select(cond, two, zero);
                self.set_ac_cc(cc);
                let taken = addr.wrapping_add(4).wrapping_add(disp13(op)) & !3;
                let t = self.iconst32(taken);
                let f = self.iconst32(addr.wrapping_add(4));
                let ip = self.fb.ins().select(cond, t, f);
                self.exit_with(ip, addr);
                Flow::End
            }
            0x31..=0x36 | 0x39..=0x3e => {
                // cmpob<cc> / cmpib<cc>
                self.prologue(4);
                let t1 = self.csrc1(op);
                let t2 = self.csrc2(op);
                let cc = self.cmp_cc(op_idx >= 0x39, t1, t2);
                self.set_ac_cc(cc);
                let mask = if op_idx >= 0x39 {
                    op_idx - 0x38
                } else {
                    op_idx - 0x30
                };
                let m = self.fb.ins().band_imm(cc, mask as i64);
                let cond = self.fb.ins().icmp_imm(IntCC::NotEqual, m, 0);
                let taken = addr.wrapping_add(4).wrapping_add(disp13(op)) & !3;
                let t = self.iconst32(taken);
                let f = self.iconst32(addr.wrapping_add(4));
                let ip = self.fb.ins().select(cond, t, f);
                self.exit_with(ip, addr);
                Flow::End
            }

            // --- test<cc> ---
            0x20..=0x27 => {
                self.prologue(1);
                let ac = self.load_ac();
                let val = if op_idx == 0x20 {
                    let m = self.fb.ins().band_imm(ac, 7);
                    let c = self.fb.ins().icmp_imm(IntCC::Equal, m, 0);
                    self.fb.ins().uextend(types::I32, c)
                } else {
                    let m = self.fb.ins().band_imm(ac, (op_idx - 0x20) as i64);
                    let c = self.fb.ins().icmp_imm(IntCC::NotEqual, m, 0);
                    self.fb.ins().uextend(types::I32, c)
                };
                self.store_r(dst, val);
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x58: bitwise logic ---
            0x58 => {
                let cost = if matches!(sub, 0x0 | 0x3 | 0xc | 0xf) { 2 } else { 1 };
                self.prologue(cost);
                if sub != 0x5 {
                    let t1 = self.src1(op);
                    let t2 = self.src2(op);
                    let one = self.iconst32(1);
                    let bitn = self.fb.ins().band_imm(t1, 31);
                    let bit = self.fb.ins().ishl(one, bitn);
                    let res = match sub {
                        0x0 => self.fb.ins().bxor(t2, bit),
                        0x1 => self.fb.ins().band(t2, t1),
                        0x2 => {
                            let n = self.fb.ins().bnot(t1);
                            self.fb.ins().band(t2, n)
                        }
                        0x3 => self.fb.ins().bor(t2, bit),
                        0x4 => {
                            let n = self.fb.ins().bnot(t2);
                            self.fb.ins().band(n, t1)
                        }
                        0x6 => self.fb.ins().bxor(t2, t1),
                        0x7 => self.fb.ins().bor(t2, t1),
                        0x8 => {
                            let n2 = self.fb.ins().bnot(t2);
                            let n1 = self.fb.ins().bnot(t1);
                            self.fb.ins().band(n2, n1)
                        }
                        0x9 => {
                            let x = self.fb.ins().bxor(t2, t1);
                            self.fb.ins().bnot(x)
                        }
                        0xa => self.fb.ins().bnot(t1),
                        0xb => {
                            let n = self.fb.ins().bnot(t1);
                            self.fb.ins().bor(t2, n)
                        }
                        0xc => {
                            let n = self.fb.ins().bnot(bit);
                            self.fb.ins().band(t2, n)
                        }
                        0xd => {
                            let n = self.fb.ins().bnot(t2);
                            self.fb.ins().bor(n, t1)
                        }
                        0xe => {
                            let n2 = self.fb.ins().bnot(t2);
                            let n1 = self.fb.ins().bnot(t1);
                            self.fb.ins().bor(n2, n1)
                        }
                        0xf => {
                            // alterbit: the previous "equal" cc picks set/clear.
                            let ac = self.load_ac();
                            let e = self.fb.ins().band_imm(ac, 2);
                            let c = self.fb.ins().icmp_imm(IntCC::NotEqual, e, 0);
                            let s = self.fb.ins().bor(t2, bit);
                            let nb = self.fb.ins().bnot(bit);
                            let cl = self.fb.ins().band(t2, nb);
                            self.fb.ins().select(c, s, cl)
                        }
                        _ => unreachable!(),
                    };
                    self.store_r(dst, res);
                }
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x59: add / subtract / shift / rotate ---
            0x59 => {
                self.prologue(1);
                if matches!(sub, 0x0..=0x3 | 0x8 | 0xa..=0xe) {
                    let t1 = self.src1(op);
                    let t2 = self.src2(op);
                    let big = self
                        .fb
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, t1, 32);
                    let sh = self.fb.ins().band_imm(t1, 31);
                    let res = match sub {
                        0x0 | 0x1 => self.fb.ins().iadd(t2, t1),
                        0x2 | 0x3 => self.fb.ins().isub(t2, t1),
                        0x8 => {
                            // shro: a shift of 32 or more yields 0.
                            let s = self.fb.ins().ushr(t2, sh);
                            let z = self.iconst32(0);
                            self.fb.ins().select(big, z, s)
                        }
                        0xa => {
                            // shrdi: arithmetic shift that rounds a negative,
                            // inexact result toward zero.
                            let s = self.fb.ins().sshr(t2, sh);
                            let one = self.iconst32(1);
                            let m1 = self.fb.ins().ishl(one, sh);
                            let m = self.fb.ins().iadd_imm(m1, -1);
                            let rem = self.fb.ins().band(t2, m);
                            let neg = self.fb.ins().icmp_imm(IntCC::SignedLessThan, t2, 0);
                            let rnz = self.fb.ins().icmp_imm(IntCC::NotEqual, rem, 0);
                            let both = self.fb.ins().band(neg, rnz);
                            let adj = self.fb.ins().uextend(types::I32, both);
                            let r = self.fb.ins().iadd(s, adj);
                            let z = self.iconst32(0);
                            self.fb.ins().select(big, z, r)
                        }
                        0xb => {
                            // shri: a shift of 32 or more saturates to the sign.
                            let s = self.fb.ins().sshr(t2, sh);
                            let neg = self.fb.ins().icmp_imm(IntCC::SignedLessThan, t2, 0);
                            let ones = self.iconst32(u32::MAX);
                            let z = self.iconst32(0);
                            let sat = self.fb.ins().select(neg, ones, z);
                            self.fb.ins().select(big, sat, s)
                        }
                        0xc | 0xe => {
                            let s = self.fb.ins().ishl(t2, sh);
                            let z = self.iconst32(0);
                            self.fb.ins().select(big, z, s)
                        }
                        0xd => self.fb.ins().rotl(t2, sh),
                        _ => unreachable!(),
                    };
                    self.store_r(dst, res);
                }
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x5A: compare family ---
            0x5A => {
                match sub {
                    0x0 | 0x1 | 0x2 | 0x3 => {
                        self.prologue(1);
                        let t1 = self.src1(op);
                        let t2 = self.src2(op);
                        if sub <= 1 {
                            let cc = self.cmp_cc(sub == 1, t1, t2);
                            self.set_ac_cc(cc);
                        } else {
                            // concmpo/concmpi: only compare with carry clear,
                            // and never produce "less".
                            let le = if sub == 3 {
                                self.fb.ins().icmp(IntCC::SignedLessThanOrEqual, t1, t2)
                            } else {
                                self.fb.ins().icmp(IntCC::UnsignedLessThanOrEqual, t1, t2)
                            };
                            let two = self.iconst32(2);
                            let one = self.iconst32(1);
                            let cc = self.fb.ins().select(le, two, one);
                            let ac = self.load_ac();
                            let carry = self.fb.ins().band_imm(ac, 4);
                            let skip = self.fb.ins().icmp_imm(IntCC::NotEqual, carry, 0);
                            let cleared = self.fb.ins().band_imm(ac, 0xFFFF_FFF8);
                            let cand = self.fb.ins().bor(cleared, cc);
                            let v = self.fb.ins().select(skip, ac, cand);
                            self.store_ac(v);
                        }
                    }
                    0x4..=0x7 => {
                        // cmpinc*/cmpdec*: compare, then bump the destination.
                        self.prologue(2);
                        let t1 = self.src1(op);
                        let t2 = self.src2(op);
                        let cc = self.cmp_cc(sub & 1 == 1, t1, t2);
                        self.set_ac_cc(cc);
                        let v = if sub <= 5 {
                            self.fb.ins().iadd_imm(t2, 1)
                        } else {
                            self.fb.ins().iadd_imm(t2, -1)
                        };
                        self.store_r(dst, v);
                    }
                    0xc => {
                        // scanbyte: "equal" when any byte lane matches.
                        self.prologue(2);
                        let t1 = self.src1(op);
                        let t2 = self.src2(op);
                        let x = self.fb.ins().bxor(t1, t2);
                        let b0 = self.fb.ins().band_imm(x, 0xFF);
                        let mut hit = self.fb.ins().icmp_imm(IntCC::Equal, b0, 0);
                        for i in 1..4 {
                            let bi = self.fb.ins().band_imm(x, 0xFFi64 << (i * 8));
                            let m = self.fb.ins().icmp_imm(IntCC::Equal, bi, 0);
                            hit = self.fb.ins().bor(hit, m);
                        }
                        let two = self.iconst32(2);
                        let zero = self.iconst32(0);
                        let cc = self.fb.ins().select(hit, two, zero);
                        self.set_ac_cc(cc);
                    }
                    0xe => {
                        // chkbit
                        self.prologue(2);
                        let t1 = self.src1(op);
                        let b = self.fb.ins().band_imm(t1, 31);
                        let t2 = self.src2(op);
                        let one = self.iconst32(1);
                        let mask = self.fb.ins().ishl(one, b);
                        let set = self.fb.ins().band(t2, mask);
                        let c = self.fb.ins().icmp_imm(IntCC::NotEqual, set, 0);
                        let two = self.iconst32(2);
                        let zero = self.iconst32(0);
                        let cc = self.fb.ins().select(c, two, zero);
                        self.set_ac_cc(cc);
                    }
                    _ => {
                        // Unassigned sub-opcodes are free no-ops.
                        self.prologue(0);
                    }
                }
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x5B: addc / subc ---
            0x5B if sub == 0x0 || sub == 0x2 => {
                self.prologue(1);
                let t1 = self.src1(op);
                let t2 = self.src2(op);
                let ac = self.load_ac();
                let carry = self.fb.ins().ushr_imm(ac, 1);
                let carry = self.fb.ins().band_imm(carry, 1);
                let (res, cout) = if sub == 0x0 {
                    let (v0, c0) = self.fb.ins().uadd_overflow(t2, t1);
                    let (r, c1) = self.fb.ins().uadd_overflow(v0, carry);
                    (r, self.fb.ins().bor(c0, c1))
                } else {
                    let (v0, b0) = self.fb.ins().usub_overflow(t2, t1);
                    let (r, b1) = self.fb.ins().usub_overflow(v0, carry);
                    (r, self.fb.ins().bor(b0, b1))
                };
                // Overflow: same rule the interpreter uses.
                let (x1, x2) = if sub == 0x0 {
                    (self.fb.ins().bxor(res, t1), self.fb.ins().bxor(res, t2))
                } else {
                    (self.fb.ins().bxor(t2, t1), self.fb.ins().bxor(t2, res))
                };
                let both = self.fb.ins().band(x1, x2);
                let ovf = self.fb.ins().ushr_imm(both, 31);
                let c32 = self.fb.ins().uextend(types::I32, cout);
                let cbit = self.fb.ins().ishl_imm(c32, 1);
                let flags = self.fb.ins().bor(cbit, ovf);
                let cleared = self.fb.ins().band_imm(ac, 0xFFFF_FFFC);
                let v = self.fb.ins().bor(cleared, flags);
                self.store_ac(v);
                self.store_r(dst, res);
                Flow::Next(addr.wrapping_add(4))
            }
            0x5B => {
                self.prologue(0);
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x5C-0x5F: mov / movl / movt / movq ---
            0x5C if sub == 0xc => {
                self.prologue(2);
                let v = self.src1(op);
                self.store_r(dst, v);
                Flow::Next(addr.wrapping_add(4))
            }
            0x5D..=0x5F if sub == 0xc => {
                self.prologue(2);
                let (count, align) = match op_idx {
                    0x5D => (2u32, 0x1eu32),
                    0x5E => (3, 0x1c),
                    _ => (4, 0x1c),
                };
                let d = dst & align;
                if op & 0x800 != 0 {
                    // Literal: every destination gets the same value.
                    let lit = self.iconst32(op & 0x1f);
                    for i in 0..count {
                        self.store_r(d + i, lit);
                    }
                } else {
                    let src = op & 0x1f;
                    // Sequential, so overlapping ranges behave like the
                    // interpreter's element-by-element copy.
                    for i in 0..count {
                        let v = self.load_r(src + i);
                        self.store_r(d + i, v);
                    }
                }
                Flow::Next(addr.wrapping_add(4))
            }
            0x5C..=0x5F => {
                self.prologue(0);
                Flow::Next(addr.wrapping_add(4))
            }

            // --- 0x70/0x74: mulo / muli. The divide/remainder ops can fault,
            // so they go through the fallback and end the block. ---
            0x70 | 0x74 => match sub {
                0x1 => {
                    self.prologue(18);
                    let t1 = self.src1(op);
                    let t2 = self.src2(op);
                    let v = self.fb.ins().imul(t2, t1);
                    self.store_r(dst, v);
                    Flow::Next(addr.wrapping_add(4))
                }
                0x8 | 0x9 | 0xb if (op_idx == 0x70 && sub != 0x9) || op_idx == 0x74 => {
                    self.fallback_end(addr)
                }
                _ => {
                    self.prologue(0);
                    Flow::Next(addr.wrapping_add(4))
                }
            },

            // --- Memory ---
            0x80 | 0xc0 => {
                // ldob / ldib. The register write is unconditional in the
                // interpreter (it happens before the stall is noticed), so it
                // is unconditional here too.
                self.prologue(4);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                let v = self.call_read(self.fr.read_byte, ea);
                let v = if op_idx == 0xc0 {
                    let s = self.fb.ins().ishl_imm(v, 24);
                    self.fb.ins().sshr_imm(s, 24)
                } else {
                    v
                };
                self.store_r(dst, v);
                self.stall_check();
                Flow::Next(next)
            }
            0x88 | 0x90 => {
                // ldos / ld: on a stall the destination keeps its old value.
                self.prologue(4);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                let f = if op_idx == 0x88 {
                    self.fr.read_u16
                } else {
                    self.fr.read_u32
                };
                let v = self.call_read(f, ea);
                self.stall_check();
                self.store_r(dst, v);
                Flow::Next(next)
            }
            0xc8 => {
                // ldis
                self.prologue(4);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                let v = self.call_read(self.fr.read_u16, ea);
                let s = self.fb.ins().ishl_imm(v, 16);
                let v = self.fb.ins().sshr_imm(s, 16);
                self.store_r(dst, v);
                self.stall_check();
                Flow::Next(next)
            }
            0x82 | 0x92 | 0xc2 | 0x8a | 0xca => {
                // stob / st / stib / stos / stis
                self.prologue(2);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                let v = self.load_r(dst);
                match op_idx {
                    0x82 | 0xc2 => self.call_write(self.fr.write_byte, ea, v),
                    0x92 => self.call_write(self.fr.write_u32, ea, v),
                    _ => self.call_write(self.fr.write_u16, ea, v),
                }
                self.stall_check();
                Flow::Next(next)
            }
            0x8c => {
                // lda
                self.prologue(1);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                self.store_r(dst, ea);
                Flow::Next(next)
            }
            0x84 => {
                // bx
                self.prologue(3);
                self.store_pip(addr);
                let (ea, _next) = self.ea(addr, op);
                self.exit_with(ea, addr);
                Flow::End
            }
            0x85 => {
                // balx: the link register gets the address *after* any
                // displacement word the EA consumed.
                self.prologue(5);
                self.store_pip(addr);
                let (ea, next) = self.ea(addr, op);
                let link = self.iconst32(next);
                self.store_r(dst, link);
                self.exit_with(ea, addr);
                Flow::End
            }

            // Burst transfers (stall mid-burst, restart at the same IP) and
            // callx go through the fallback and end the block.
            0x98 | 0x9a | 0xa0 | 0xa2 | 0xb0 | 0xb2 | 0x86 => self.fallback_end(addr),

            // --- FPU: exact via the interpreter, but straight-line. ---
            0x67..=0x6e | 0x78..=0x7f => self.fallback_continue(addr),

            // --- call / ret / synmov / complex system ops: fallback + end. ---
            0x09 | 0x0a | 0x60 | 0x61 | 0x64..=0x66 => self.fallback_end(addr),

            // 0x38 and the mem/op ranges' unassigned encodings are free
            // no-ops in the interpreter (matched range, unmatched arm).
            0x38 => {
                self.prologue(0);
                Flow::Next(addr.wrapping_add(4))
            }
            0x81 | 0x83 | 0x87 | 0x89 | 0x8b | 0x8d..=0x8f | 0x91 | 0xa1 | 0xa3..=0xaf
            | 0xb1 | 0xb3..=0xbf | 0xc1 | 0xc3..=0xc7 | 0xc9 | 0xcb => {
                self.prologue(0);
                Flow::Next(addr.wrapping_add(4))
            }
            // op_int range encodings not matched there (0x5A/0x5B/0x5C-0x5F
            // unassigned subs handled above; nothing else reaches here), and
            // everything outside every dispatch range: the dispatcher's
            // catch-all charges 1 cycle and does nothing.
            _ => {
                self.prologue(1);
                Flow::Next(addr.wrapping_add(4))
            }
        }
    }
}

impl I960Cpu {
    /// Dynarec counterpart of `execute_run`. Compiled blocks run until the
    /// cycle budget is spent or a memory access stalls; between blocks the
    /// same IRQ-line sampling the interpreter does per instruction happens
    /// per block, which the block length cap keeps equivalently timely.
    ///
    /// Breakpoints and the trace ring need per-instruction visibility, so
    /// while either is active this defers to the interpreter wholesale.
    pub fn execute_run_jit<B: Bus + 'static>(&mut self, bus: &mut B, cycles: i32) {
        if !self.breakpoints.is_empty() || self.trace.is_some() {
            self.execute_run(bus, cycles);
            return;
        }

        let mut jit: Box<Jit<B>> = match self.jit.0.take().and_then(|a| a.downcast::<Jit<B>>().ok())
        {
            Some(j) => j,
            None => Jit::new(),
        };

        self.icount = cycles;

        if self.deferred_vector != 0 {
            let vec = self.deferred_vector;
            self.deferred_vector = 0;
            self.request_irq_vector(bus, vec);
        }

        if !self.stall_state.burst_mode {
            self.check_pending_irqs(bus);
            self.pending_irq_check = false;
            self.check_immediate_irqs(bus);
        }

        while self.icount > 0 {
            if let Some(lines) = bus.take_irq_lines() {
                for (i, state) in lines.iter().enumerate() {
                    self.set_irq_line(i, *state);
                }
            }
            let f = jit.block_for(bus, self.ip);
            let status = unsafe { f(self as *mut I960Cpu, bus as *mut B as *mut c_void) };
            if status != 0 {
                break;
            }
        }

        // Read once: an env lookup per quantum was showing up in profiles.
        static STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *STATS.get_or_init(|| std::env::var_os("I960_JIT_STATS").is_some()) {
            eprintln!(
                "jit: {} blocks compiled, {} recompiled after code writes",
                jit.compiles, jit.recompiles
            );
        }
        self.jit.0 = Some(jit);
    }
}
