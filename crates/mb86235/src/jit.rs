//! Cranelift block dynarec for the Fujitsu MB86235 "TGPx4", with the
//! interpreter as the exact fallback for everything not lowered natively.
//! Mirrors the SHARC JIT's architecture (`crates/sharc/src/jit.rs`):
//!
//! * A **block** is a run of guest instructions starting at `pc` with
//! * sequential fetch. Compilation stops at the first instruction that can
//!   redirect control flow (DJMP/DBcc/DCcc/DCALL/DRET/DBLP/DBBC/DBBS) or arm
//!   a REP (the repeat holds the PC at runtime, so sequential addresses are
//!   no longer compile-time known), and at a 32-instruction cap. The driver
//!   runs the interpreter's `step` for delay-slot instructions, for REP
//!   repetitions, and for input-FIFO stall retries -- the states where the
//!   fetch address is not simply `pc`.
//! * Blocks are cached by entry address. All guest state lives in the
//!   `Mb86235` struct in memory (reached through `offset_of!` offsets), so a
//!   block can call back into Rust at any point with the architected state
//!   consistent. `icount`/`insns` are charged at each exit by a compile-time
//!   constant; the control operations' extra `icount` adjustments happen
//!   inside the interpreter helpers, exactly as in the interpreter loop.
//! * Every instruction with side effects goes through a trampoline
//!   (`t_exec`) that runs the interpreter's per-instruction bookkeeping
//!   (ppc, the delay-slot/REP PC sequencing) and its own `execute_op`
//!   verbatim -- "not lowered natively" never means "behaves differently".
//!   After each trampoline the `stalled` flag is checked inline; a stall
//!   ends the block with `pc`/`stall_pc` consistent, and the driver retries
//!   through `step` like the interpreter loop does.
//! * Natively lowered (no trampoline): the control-class NOP (free), the
//!   illegal class (a counted fault, one `unimpl` increment), the
//!   immediate-to-register transfer (class 7), and the hot geometry forms --
//!   classes 0/1/4/5 with register-only transfers (no FIFO, no external bus,
//!   no EA post-increments) whose ALU slot is one of the float/integer
//!   arithmetic, compare, logical or shift operations and whose multiplier
//!   slot is FMUL/IMUL, with sources kept off the PR ring (its reads have
//!   pointer post-actions). Flag updates (`st`) reproduce the interpreter's
//!   exact set/clear/sticky semantics; host f32 arithmetic is bit-identical
//!   to the interpreter's own f32 operations on x86-64. The win over the
//!   interpreter loop is the hoisted fetch/decode (compile-time), the
//!   inlined register/flag arithmetic, and the vectorized cycle accounting.
//! * Uploaded microcode: every `upload_program_half` bumps a per-page epoch
//!   (`Mb86235::code_epochs`); a cached block records the epochs of the
//!   pages it spans and is recompiled when one moves.
//!
//! The bus is type-erased through a vtable (`Mb86235Vtable`) rather than by
//! monomorphizing the cache over `B: Mb86235Bus`, because one of the two bus
//! implementations (the coprocessor worker's `BatchBus<'_>`) borrows and is
//! not `'static`. Lowering decisions follow the same basic-block contract as
//! MAME's BSD-3 MB86235 DRC front-end
//! (`src/devices/cpu/mb86235/mb86235drc.cpp`): end blocks at control flow,
//! fall back to the reference handlers for anything not lowered; our
//! interpreter's semantics remain the authority for behaviour.

// cranelift 0.134 renamed the `_imm` builders to `_imm_s`/`_imm_u`; the old
// names keep their sign-extending behaviour, which is what the exits want.
#![allow(deprecated)]

use std::ffi::c_void;
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::immediates::Ieee32;
use cranelift_codegen::ir::{self, types, AbiParam, FuncRef, InstBuilder, MemFlagsData, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use crate::Mb86235Bus;
use crate::ops::{Class, classify};
use crate::state::{Mb86235, PROGRAM_WORDS, flag};

/// Instructions per block cap. Bounds the quantum overshoot (the interpreter
/// stops mid-block at `icount == 0`; the JIT charges at block exits).
const MAX_BLOCK_INSNS: usize = 32;

/// Dev-only override for the block length cap, used by the dual-run test:
/// single-instruction blocks make the JIT stop at exactly the cycle counts
/// the interpreter does, so the two can be diffed in lockstep. 0 = no
/// override.
pub static BLOCK_CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Compiled block entry: guest CPU, type-erased bus, bus vtable. Returns 0.
type BlockFn = unsafe extern "C" fn(*mut Mb86235, *mut c_void, *const Mb86235Vtable) -> i32;

const OFF_PC: i32 = offset_of!(Mb86235, pc) as i32;
const OFF_PPC: i32 = offset_of!(Mb86235, ppc) as i32;
const OFF_STALLED: i32 = offset_of!(Mb86235, stalled) as i32;
const OFF_ICOUNT: i32 = offset_of!(Mb86235, icount) as i32;
const OFF_INSNS: i32 = offset_of!(Mb86235, insns) as i32;
const OFF_UNIMPL: i32 = offset_of!(Mb86235, unimpl) as i32;
const OFF_AA: i32 = offset_of!(Mb86235, aa) as i32;
const OFF_AB: i32 = offset_of!(Mb86235, ab) as i32;
const OFF_MA: i32 = offset_of!(Mb86235, ma) as i32;
const OFF_MB: i32 = offset_of!(Mb86235, mb) as i32;
const OFF_AR: i32 = offset_of!(Mb86235, ar) as i32;
const OFF_EB: i32 = offset_of!(Mb86235, eb) as i32;
const OFF_EO: i32 = offset_of!(Mb86235, eo) as i32;
const OFF_SP: i32 = offset_of!(Mb86235, sp) as i32;
const OFF_ST: i32 = offset_of!(Mb86235, st) as i32;
const OFF_MOD: i32 = offset_of!(Mb86235, mod_) as i32;
const OFF_LPC: i32 = offset_of!(Mb86235, lpc) as i32;

// --- bus vtable (type erasure over B: Mb86235Bus) ----------------------------

/// Function pointers for one `Mb86235Bus` implementation, so compiled blocks
/// and their Rust helpers are bus-type-agnostic. `fifo_in_pop` packs its
/// `Option<u32>` as `valid << 32 | value`.
pub struct Mb86235Vtable {
    data_read: unsafe extern "C" fn(*mut c_void, u32) -> u32,
    data_write: unsafe extern "C" fn(*mut c_void, u32, u32),
    fifo_in_pop: unsafe extern "C" fn(*mut c_void) -> u64,
    fifo_out_push: unsafe extern "C" fn(*mut c_void, u32),
    fifo_in_empty: unsafe extern "C" fn(*mut c_void) -> u32,
    fifo_in_full: unsafe extern "C" fn(*mut c_void) -> u32,
    fifo_out_empty: unsafe extern "C" fn(*mut c_void) -> u32,
    fifo_out_full: unsafe extern "C" fn(*mut c_void) -> u32,
}

unsafe extern "C" fn vt_data_read<B: Mb86235Bus>(p: *mut c_void, a: u32) -> u32 {
    unsafe { (*(p as *mut B)).data_read(a) }
}
unsafe extern "C" fn vt_data_write<B: Mb86235Bus>(p: *mut c_void, a: u32, d: u32) {
    unsafe { (*(p as *mut B)).data_write(a, d) }
}
unsafe extern "C" fn vt_fifo_in_pop<B: Mb86235Bus>(p: *mut c_void) -> u64 {
    match unsafe { (*(p as *mut B)).fifo_in_pop() } {
        Some(v) => (1u64 << 32) | v as u64,
        None => 0,
    }
}
unsafe extern "C" fn vt_fifo_out_push<B: Mb86235Bus>(p: *mut c_void, d: u32) {
    unsafe { (*(p as *mut B)).fifo_out_push(d) }
}
unsafe extern "C" fn vt_fifo_in_empty<B: Mb86235Bus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_in_empty() as u32 }
}
unsafe extern "C" fn vt_fifo_in_full<B: Mb86235Bus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_in_full() as u32 }
}
unsafe extern "C" fn vt_fifo_out_empty<B: Mb86235Bus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_out_empty() as u32 }
}
unsafe extern "C" fn vt_fifo_out_full<B: Mb86235Bus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_out_full() as u32 }
}

/// Builds the vtable for one bus type (eight function pointers; assembled on
/// the caller's stack per `execute` batch).
fn vtable<B: Mb86235Bus>() -> Mb86235Vtable {
    Mb86235Vtable {
        data_read: vt_data_read::<B>,
        data_write: vt_data_write::<B>,
        fifo_in_pop: vt_fifo_in_pop::<B>,
        fifo_out_push: vt_fifo_out_push::<B>,
        fifo_in_empty: vt_fifo_in_empty::<B>,
        fifo_in_full: vt_fifo_in_full::<B>,
        fifo_out_empty: vt_fifo_out_empty::<B>,
        fifo_out_full: vt_fifo_out_full::<B>,
    }
}

/// A `Mb86235Bus` that forwards through the vtable; the helpers build one on
/// the stack and then run the interpreter's own code verbatim.
struct BusPtr {
    bus: *mut c_void,
    vt: *const Mb86235Vtable,
}

impl Mb86235Bus for BusPtr {
    fn data_read(&mut self, addr: u32) -> u32 {
        unsafe { ((*self.vt).data_read)(self.bus, addr) }
    }
    fn data_write(&mut self, addr: u32, data: u32) {
        unsafe { ((*self.vt).data_write)(self.bus, addr, data) }
    }
    fn fifo_in_pop(&mut self) -> Option<u32> {
        let r = unsafe { ((*self.vt).fifo_in_pop)(self.bus) };
        if r >> 32 != 0 { Some(r as u32) } else { None }
    }
    fn fifo_out_push(&mut self, data: u32) {
        unsafe { ((*self.vt).fifo_out_push)(self.bus, data) }
    }
    fn fifo_in_empty(&self) -> bool {
        unsafe { ((*self.vt).fifo_in_empty)(self.bus) != 0 }
    }
    fn fifo_in_full(&self) -> bool {
        unsafe { ((*self.vt).fifo_in_full)(self.bus) != 0 }
    }
    fn fifo_out_empty(&self) -> bool {
        unsafe { ((*self.vt).fifo_out_empty)(self.bus) != 0 }
    }
    fn fifo_out_full(&self) -> bool {
        unsafe { ((*self.vt).fifo_out_full)(self.bus) != 0 }
    }
}

// --- Rust callbacks reachable from compiled code -----------------------------

/// The fallback every non-trivial instruction goes through: the interpreter's
/// per-instruction PC bookkeeping followed by its own `execute_op` for the
/// already-fetched word. `curpc` is a compile-time constant inside a block;
/// the helper leaves the architected `pc`/`ppc` in memory exactly where the
/// interpreter loop would.
unsafe extern "C" fn t_exec(
    cpu: *mut Mb86235,
    bus: *mut c_void,
    vt: *const Mb86235Vtable,
    curpc: i32,
    op: i64,
) {
    let c = unsafe { &mut *cpu };
    let mut b = BusPtr { bus, vt };
    let curpc = curpc as u32;
    c.ppc = curpc;
    if c.delay_slot {
        c.pc = c.delay_pc;
        c.delay_slot = false;
    } else if c.st & flag::RP != 0 {
        c.rpc = c.rpc.wrapping_sub(1);
        if c.rpc == 1 {
            c.st &= !flag::RP;
        }
    } else {
        c.pc = curpc.wrapping_add(1);
    }
    c.execute_op(&mut b, op as u64);
}

struct HelperIds {
    exec: FuncId,
}

struct FuncRefs {
    exec: FuncRef,
}

#[derive(Clone, Copy)]
struct CompiledBlock {
    f: BlockFn,
    /// Entry address this block was compiled for (direct-mapped cache tag).
    tag: u32,
    // Program-RAM pages (addr >> 8) the block spans, and their code epochs at
    // compile time. An upload to either page recompiles.
    p0: usize,
    p1: usize,
    e0: u64,
    e1: u64,
}

/// Block cache size (direct-mapped L1 over a HashMap backing store, same
/// shape as the SHARC cache).
const CACHE_BITS: u32 = 13;
const CACHE_SIZE: usize = 1 << CACHE_BITS;

pub struct Jit {
    module: JITModule,
    helpers: HelperIds,
    block_sig: ir::Signature,
    fb_ctx: FunctionBuilderContext,
    l1: Vec<Option<CompiledBlock>>,
    cache: std::collections::HashMap<u32, CompiledBlock>,
    counter: u64,
    /// Diagnostics: blocks compiled, and recompiles caused by code uploads.
    pub compiles: u64,
    pub recompiles: u64,
}

impl Jit {
    pub fn new() -> Box<Self> {
        let mut flags = settings::builder();
        flags.set("opt_level", "none").expect("opt_level");
        flags
            .set("enable_verifier", "false")
            .expect("enable_verifier");
        let isa = cranelift_native::builder()
            .expect("host ISA")
            .finish(settings::Flags::new(flags))
            .expect("ISA");
        let mut builder =
            JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("mb86235_exec", t_exec as *const u8);
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
        let exec = sig(
            &[ptr, ptr, ptr, types::I32, types::I64],
            &[],
            &mut module,
        );
        let block_sig = sig(&[ptr, ptr, ptr], &[types::I32], &mut module);

        let helpers = HelperIds {
            exec: module
                .declare_function("mb86235_exec", Linkage::Import, &exec)
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
        })
    }

    /// The block for `entry`, compiling (or recompiling after an upload) as
    /// needed. Program RAM is always internal, so every entry compiles.
    fn block_for(&mut self, cpu: &Mb86235, entry: u32) -> BlockFn {
        let slot = (entry as usize) & (CACHE_SIZE - 1);
        if let Some(b) = self.l1[slot] {
            if b.tag == entry && self.epochs_match(cpu, &b) {
                return b.f;
            }
        }
        match self.cache.get(&entry) {
            Some(b) if self.epochs_match(cpu, b) => {
                let b = *b;
                self.l1[slot] = Some(b);
                b.f
            }
            _ => {
                if self.cache.contains_key(&entry) {
                    self.recompiles += 1;
                }
                self.compiles += 1;
                let b = self.compile(cpu, entry);
                let f = b.f;
                self.cache.insert(entry, b);
                self.l1[slot] = Some(b);
                f
            }
        }
    }

    fn epochs_match(&self, cpu: &Mb86235, b: &CompiledBlock) -> bool {
        cpu.code_epochs[b.p0] == b.e0 && (b.p1 == b.p0 || cpu.code_epochs[b.p1] == b.e1)
    }

    fn compile(&mut self, cpu: &Mb86235, entry: u32) -> CompiledBlock {
        self.counter += 1;
        let name = format!("mb86235_blk_{entry:06x}_{}", self.counter);
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

        let mut last: u32;
        let mut n = 0u32;
        {
            let mut fb = FunctionBuilder::new(&mut ctx.func, fb_ctx);
            let fr = FuncRefs {
                exec: module.declare_func_in_func(helpers.exec, fb.func),
            };

            let entry_bb = fb.create_block();
            fb.append_block_params_for_function_params(entry_bb);
            fb.switch_to_block(entry_bb);
            fb.seal_block(entry_bb);
            let cpu_v = fb.block_params(entry_bb)[0];
            let busp = fb.block_params(entry_bb)[1];
            let vt = fb.block_params(entry_bb)[2];

            let mut e = Emit {
                fb,
                fr: &fr,
                cpu: cpu_v,
                busp,
                vt,
            };

            let cap = match BLOCK_CAP.load(std::sync::atomic::Ordering::Relaxed) {
                0 => MAX_BLOCK_INSNS,
                n => n,
            };
            let mut addr = entry;
            loop {
                let op = cpu.program[(addr as usize) & (PROGRAM_WORDS - 1)];
                let end = ends_block(op);
                if is_free_nop(op) {
                    // No architectural effect beyond the PC bookkeeping.
                    e.store_ppc(addr);
                } else if matches!(classify(op), Class::Illegal) {
                    // A counted fault, exactly `unimpl[3] += 1`.
                    e.store_ppc(addr);
                    e.bump_unimpl();
                } else if e.try_lower(addr, op) {
                    // Lowered natively; the register-only subset cannot stall.
                } else {
                    e.call_exec(addr, op);
                    // A FIFO stall ends the block: the driver retries this
                    // instruction through the interpreter's `step`.
                    let stalled = e.fb.ins().load(types::I8, flags(), e.cpu, OFF_STALLED);
                    let is_stalled = e.fb.ins().icmp_imm(IntCC::NotEqual, stalled, 0);
                    let stall_bb = e.fb.create_block();
                    let cont_bb = e.fb.create_block();
                    e.fb.ins().brif(is_stalled, stall_bb, &[], cont_bb, &[]);
                    e.fb.switch_to_block(stall_bb);
                    e.exit(addr.wrapping_add(1), n + 1);
                    e.fb.seal_block(stall_bb);
                    e.fb.switch_to_block(cont_bb);
                    e.fb.seal_block(cont_bb);
                }
                n += 1;
                last = addr;
                if end || n as usize >= cap {
                    e.exit(addr.wrapping_add(1), n);
                    break;
                }
                addr = addr.wrapping_add(1);
            }
            e.fb.finalize(frontend_cfg);
        }

        module
            .define_function(fid, &mut ctx)
            .expect("mb86235 jit: define_function");
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize");
        let ptr = module.get_finalized_function(fid);

        let p0 = page(entry);
        let p1 = page(last);
        CompiledBlock {
            f: unsafe { std::mem::transmute::<*const u8, BlockFn>(ptr) },
            tag: entry,
            p0,
            p1,
            e0: cpu.code_epochs[p0],
            e1: cpu.code_epochs[p1],
        }
    }
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// Code-epoch page of a program address (256-word pages).
fn page(addr: u32) -> usize {
    ((addr as usize) & (PROGRAM_WORDS - 1)) >> 8
}

/// The block emitter.
struct Emit<'a, 'b> {
    fb: FunctionBuilder<'a>,
    fr: &'b FuncRefs,
    cpu: Value,
    busp: Value,
    vt: Value,
}

impl Emit<'_, '_> {
    fn iconst32(&mut self, v: u32) -> Value {
        self.fb.ins().iconst(types::I32, v as i32 as i64)
    }

    /// `ppc = curpc`, the half of the interpreter's per-instruction
    /// bookkeeping the inline paths still owe the architected state. (`pc`
    /// itself is stored at the block exit.)
    fn store_ppc(&mut self, addr: u32) {
        let ppc = self.iconst32(addr);
        self.fb.ins().store(flags(), ppc, self.cpu, OFF_PPC);
    }

    /// `unimpl[3] += 1` -- the whole effect of the illegal class.
    fn bump_unimpl(&mut self) {
        let slot = self
            .fb
            .ins()
            .load(types::I64, flags(), self.cpu, OFF_UNIMPL + 3 * 8);
        let slot = self.fb.ins().iadd_imm(slot, 1);
        self.fb
            .ins()
            .store(flags(), slot, self.cpu, OFF_UNIMPL + 3 * 8);
    }

    // --- native lowering of the register-form classes -------------------------

    fn ld(&mut self, off: i32) -> Value {
        self.fb.ins().load(types::I32, flags(), self.cpu, off)
    }

    fn sd(&mut self, v: Value, off: i32) {
        self.fb.ins().store(flags(), v, self.cpu, off);
    }

    fn f32c(&mut self, bits: u32) -> Value {
        self.fb.ins().f32const(Ieee32::with_bits(bits))
    }

    fn u2f(&mut self, v: Value) -> Value {
        self.fb.ins().bitcast(types::F32, flags(), v)
    }

    fn f2u(&mut self, v: Value) -> Value {
        self.fb.ins().bitcast(types::I32, flags(), v)
    }

    /// One of the 64 transfer-slot registers, read. Only the groups the
    /// lowering predicates allow: plain arrays and control registers (no FIFO,
    /// no PR ring, no side effects); groups 7+ read as 0.
    fn get_treg(&mut self, which: u8) -> Value {
        let n = (which & 7) as i32;
        match which >> 3 {
            0 => self.ld(OFF_MA + 4 * n),
            1 => self.ld(OFF_AA + 4 * n),
            2 => match n {
                0 => self.ld(OFF_EB),
                1 => {
                    let eb = self.ld(OFF_EB);
                    self.fb.ins().ushr_imm(eb, 14)
                }
                2 => {
                    let eb = self.ld(OFF_EB);
                    self.fb.ins().band_imm(eb, 0x3fff)
                }
                3 => self.ld(OFF_EO),
                4 => self.ld(OFF_SP),
                5 => self.ld(OFF_ST),
                6 => self.ld(OFF_MOD),
                _ => self.ld(OFF_LPC),
            },
            3 => self.ld(OFF_AR + 4 * n),
            4 => self.ld(OFF_MB + 4 * n),
            5 => self.ld(OFF_AB + 4 * n),
            _ => self.iconst32(0),
        }
    }

    /// The write half of `get_treg`. Group 6 (FIFO/PR/ports) never reaches
    /// here; group 7+ writes are dropped, exactly like the interpreter.
    fn set_treg(&mut self, which: u8, v: Value) {
        let n = (which & 7) as i32;
        match which >> 3 {
            0 => self.sd(v, OFF_MA + 4 * n),
            1 => self.sd(v, OFF_AA + 4 * n),
            2 => match n {
                0 => self.sd(v, OFF_EB),
                1 => {
                    let eb = self.ld(OFF_EB);
                    let keep = self.fb.ins().band_imm(eb, 0x3fff);
                    let hi = self.fb.ins().ishl_imm(v, 14);
                    let eb = self.fb.ins().bor(keep, hi);
                    self.sd(eb, OFF_EB);
                }
                2 => {
                    let eb = self.ld(OFF_EB);
                    let m = self.iconst32(0xffc000);
                    let keep = self.fb.ins().band(eb, m);
                    let hi = self.fb.ins().ishl_imm(v, 14);
                    let eb = self.fb.ins().bor(keep, hi);
                    self.sd(eb, OFF_EB);
                }
                3 => self.sd(v, OFF_EO),
                4 => self.sd(v, OFF_SP),
                5 => self.sd(v, OFF_ST),
                6 => self.sd(v, OFF_MOD),
                _ => self.sd(v, OFF_LPC),
            },
            3 => {
                let v = self.fb.ins().band_imm(v, 0x3fff);
                self.sd(v, OFF_AR + 4 * n);
            }
            4 => self.sd(v, OFF_MB + 4 * n),
            5 => self.sd(v, OFF_AB + 4 * n),
            _ => {}
        }
    }

    /// ALU-slot source register (aa/ab banks or a compile-time constant; the
    /// PR-ring group is excluded by the lowering predicates).
    fn get_alureg(&mut self, which: u8, isfloat: bool) -> Value {
        let n = (which & 7) as i32;
        match which >> 3 {
            0 => self.ld(OFF_AA + 4 * n),
            1 => self.ld(OFF_AB + 4 * n),
            _ => {
                let bits = if isfloat {
                    const_float_bits(which & 7)
                } else {
                    const_int_bits(which & 7)
                };
                self.iconst32(bits)
            }
        }
    }

    /// Multiplier-slot source register (ma/mb banks or a constant).
    fn get_mulreg(&mut self, which: u8, isfloat: bool) -> Value {
        let n = (which & 7) as i32;
        match which >> 3 {
            0 => self.ld(OFF_MA + 4 * n),
            1 => self.ld(OFF_MB + 4 * n),
            _ => {
                let bits = if isfloat {
                    const_float_bits(which & 7)
                } else {
                    const_int_bits(which & 7)
                };
                self.iconst32(bits)
            }
        }
    }

    /// ALU/multiplier destination: ma/mb/aa/ab in group order.
    fn set_alureg(&mut self, which: u8, v: Value) {
        let n = (which & 7) as i32;
        let off = match which >> 3 {
            0 => OFF_MA + 4 * n,
            1 => OFF_MB + 4 * n,
            2 => OFF_AA + 4 * n,
            _ => OFF_AB + 4 * n,
        };
        self.sd(v, off);
    }

    // --- st flag maintenance, mirroring alu.rs exactly -------------------------

    fn st_clear(&mut self, mask: u32) {
        let s = self.ld(OFF_ST);
        let m = self.iconst32(!mask);
        let s = self.fb.ins().band(s, m);
        self.sd(s, OFF_ST);
    }

    /// `st |= bit` when `cond` (an i8 boolean) holds. Used for the sticky
    /// flags too: they are simply never cleared.
    fn st_set_if(&mut self, cond: Value, bit: u32) {
        let c = self.fb.ins().uextend(types::I32, cond);
        let c = self.fb.ins().imul_imm(c, bit as i64);
        let s = self.ld(OFF_ST);
        let s = self.fb.ins().bor(s, c);
        self.sd(s, OFF_ST);
    }

    /// set_flags_d / set_flags_i: AN from bit 31, AZ from zero.
    fn flags_int(&mut self, val: Value) {
        self.st_clear(flag::AN | flag::AZ);
        let an = self.fb.ins().icmp_imm(IntCC::SignedLessThan, val, 0);
        self.st_set_if(an, flag::AN);
        let az = self.fb.ins().icmp_imm(IntCC::Equal, val, 0);
        self.st_set_if(az, flag::AZ);
    }

    /// set_flags_f: ordered compares, so NaN sets neither AN nor AZ and a
    /// negative zero reads as zero -- the host f32 semantics the interpreter
    /// implements in Rust.
    fn flags_float(&mut self, val: Value) {
        self.st_clear(flag::AN | flag::AZ);
        let zero = self.f32c(0);
        let an = self.fb.ins().fcmp(FloatCC::LessThan, val, zero);
        self.st_set_if(an, flag::AN);
        let az = self.fb.ins().fcmp(FloatCC::Equal, val, zero);
        self.st_set_if(az, flag::AZ);
    }

    /// The ALU slot, for the opcodes in `LOWERED_ALU`. `imm` is the ai2
    /// field (the shift amount for the logical group).
    fn emit_aluop(&mut self, opcode: u8, src1: Value, src2: Value, imm: u8, dst: u8) {
        match opcode {
            0x00..=0x03 => {
                // FADD / FADDZ / FSUB / FSUBZ
                let f1 = self.u2f(src1);
                let f2 = self.u2f(src2);
                let mut d = if opcode & 2 != 0 {
                    self.fb.ins().fsub(f2, f1)
                } else {
                    self.fb.ins().fadd(f1, f2)
                };
                if opcode & 1 != 0 {
                    self.st_clear(flag::ZC);
                    let zero = self.f32c(0);
                    let neg = self.fb.ins().fcmp(FloatCC::LessThan, d, zero);
                    self.st_set_if(neg, flag::ZC);
                    d = self.fb.ins().select(neg, zero, d);
                }
                self.flags_float(d);
                let d = self.f2u(d);
                self.set_alureg(dst, d);
            }
            0x04 | 0x06 => {
                // FCMP / FABC
                let f1 = self.u2f(src1);
                let f2 = self.u2f(src2);
                let d = if opcode & 2 != 0 {
                    let a2 = self.fb.ins().fabs(f2);
                    let a1 = self.fb.ins().fabs(f1);
                    self.fb.ins().fsub(a2, a1)
                } else {
                    self.fb.ins().fsub(f2, f1)
                };
                self.flags_float(d);
            }
            0x05 => {
                let d = {
                    let f = self.u2f(src1);
                    self.fb.ins().fabs(f)
                };
                self.flags_float(d);
                let d = self.f2u(d);
                self.set_alureg(dst, d);
            }
            0x07 => {} // NOP
            0x0d => {
                // CIF: int -> float
                let f = self.fb.ins().fcvt_from_sint(types::F32, src1);
                self.flags_float(f);
                let d = self.f2u(f);
                self.set_alureg(dst, d);
            }
            0x0e => {
                // CFI: float -> int (saturating, NaN -> 0, like Rust's `as`)
                let f = self.u2f(src1);
                let v = self.fb.ins().fcvt_to_sint_sat(types::I32, f);
                self.flags_int(v);
                self.set_alureg(dst, v);
            }
            0x10..=0x13 => {
                // ADD / ADDZ / SUB / SUBZ
                let mut res = if opcode & 2 != 0 {
                    self.fb.ins().isub(src2, src1)
                } else {
                    self.fb.ins().iadd(src1, src2)
                };
                if opcode & 1 != 0 {
                    self.st_clear(flag::ZC);
                    let neg = self.fb.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
                    self.st_set_if(neg, flag::ZC);
                    let zero = self.iconst32(0);
                    res = self.fb.ins().select(neg, zero, res);
                }
                self.flags_int(res);
                self.set_alureg(dst, res);
            }
            0x14 => {
                // CMP
                let res = self.fb.ins().isub(src2, src1);
                self.flags_int(res);
            }
            0x15 => {
                // ABS
                let v = self.fb.ins().band_imm(src1, 0x7fff_ffff);
                self.flags_int(v);
                self.set_alureg(dst, v);
            }
            0x16 | 0x17 => {
                // ATR / ATRZ: no AN/AZ update.
                let mut v = src1;
                if opcode & 1 != 0 {
                    self.st_clear(flag::ZC);
                    let m = self.iconst32(0x8000_0000);
                    let top = self.fb.ins().band(src1, m);
                    let neg = self.fb.ins().icmp_imm(IntCC::NotEqual, top, 0);
                    self.st_set_if(neg, flag::ZC);
                    let zero = self.iconst32(0);
                    v = self.fb.ins().select(neg, zero, v);
                }
                self.set_alureg(dst, v);
            }
            0x18..=0x1b => {
                // AND / OR / XOR / NOT
                let r = match opcode {
                    0x18 => self.fb.ins().band(src1, src2),
                    0x19 => self.fb.ins().bor(src1, src2),
                    0x1a => self.fb.ins().bxor(src1, src2),
                    _ => self.fb.ins().bnot(src1),
                };
                self.flags_int(r);
                self.set_alureg(dst, r);
            }
            0x1c..=0x1f => {
                // SHR / SHL / SAR / SAL by imm & 31
                let sh = (imm & 31) as i64;
                let r = match opcode {
                    0x1c => self.fb.ins().ushr_imm(src1, sh),
                    0x1d => self.fb.ins().ishl_imm(src1, sh),
                    0x1e => self.fb.ins().sshr_imm(src1, sh),
                    _ => self.fb.ins().ishl_imm(src1, sh),
                };
                self.flags_int(r);
                self.set_alureg(dst, r);
            }
            _ => unreachable!("LOWERED_ALU gates emit_aluop"),
        }
    }

    /// The multiplier slot: FMUL/IMUL with the interpreter's flag semantics
    /// (MV/MU sticky, MN/MZ/MD cleared first).
    fn emit_mulop(&mut self, isfmul: bool, src1: Value, src2: Value, dst: u8) {
        if isfmul {
            let f1 = self.u2f(src1);
            let f2 = self.u2f(src2);
            let res = self.fb.ins().fmul(f1, f2);
            self.st_clear(flag::MN | flag::MZ | flag::MD);
            let zero = self.f32c(0);
            let mn = self.fb.ins().fcmp(FloatCC::LessThan, res, zero);
            self.st_set_if(mn, flag::MN);
            let mz = self.fb.ins().fcmp(FloatCC::Equal, res, zero);
            self.st_set_if(mz, flag::MZ);
            let abs = self.fb.ins().fabs(res);
            let inf = self.f32c(0x7f80_0000);
            let mv = self.fb.ins().fcmp(FloatCC::Equal, abs, inf);
            self.st_set_if(mv, flag::MV);
            let min_pos = self.f32c(0x0080_0000);
            let mu = self.fb.ins().fcmp(FloatCC::LessThan, abs, min_pos);
            self.st_set_if(mu, flag::MU);
            let md = self.fb.ins().fcmp(FloatCC::NotEqual, res, res);
            self.st_set_if(md, flag::MD);
            let res = self.f2u(res);
            self.set_alureg(dst, res);
        } else {
            let res = self.fb.ins().imul(src1, src2);
            self.st_clear(flag::MN | flag::MZ);
            let mn = self.fb.ins().icmp_imm(IntCC::SignedLessThan, res, 0);
            self.st_set_if(mn, flag::MN);
            let mz = self.fb.ins().icmp_imm(IntCC::Equal, res, 0);
            self.st_set_if(mz, flag::MZ);
            self.set_alureg(dst, res);
        }
    }

    /// The dual ALU slot (ALU op + multiply), in the interpreter's order:
    /// sources first, ALU then multiplier (a destination collision resolves
    /// for the multiplier).
    fn emit_alu2(&mut self, op: u64) {
        let aluop = aop(op);
        let s1 = self.get_alureg(ai1(op), false);
        let s2 = if Mb86235::alu_has_second_src(aluop) {
            self.get_alureg(ai2(op), aluop & 0x10 == 0)
        } else {
            self.iconst32(0)
        };
        let isfmul = mop(op) != 0;
        let m1 = self.get_mulreg(mi1(op), false);
        let m2 = self.get_mulreg(mi2(op), isfmul);
        self.emit_aluop(aluop, s1, s2, ai2(op), ao(op));
        self.emit_mulop(isfmul, m1, m2, mo(op));
    }

    /// The single ALU slot: bit 41 selects ALU or multiplier.
    fn emit_alu1(&mut self, op: u64) {
        if op & (1 << 41) != 0 {
            let aluop = aop(op);
            let s1 = self.get_alureg(ai1(op), false);
            let s2 = if Mb86235::alu_has_second_src(aluop) {
                self.get_alureg(ai2(op), aluop & 0x10 == 0)
            } else {
                self.iconst32(0)
            };
            self.emit_aluop(aluop, s1, s2, ai2(op), ao(op));
        } else {
            let isfmul = aop(op) != 0;
            let m1 = self.get_mulreg(ai1(op), false);
            let m2 = self.get_mulreg(ai2(op), isfmul);
            self.emit_mulop(isfmul, m1, m2, ao(op));
        }
    }

    /// Lowers one instruction natively when its class and operands are all in
    /// the register-only subset; returns false to fall back to the
    /// trampoline. Transfer sources are read before the ALU slot issues and
    /// destinations written after, matching the interpreter's ordering.
    fn try_lower(&mut self, addr: u32, op: u64) -> bool {
        match classify(op) {
            // Class 0, sd == 0: register -> ALU2 -> register, twice.
            Class::Alu2Trans2 if (op >> 25) & 3 == 0 && alu2_lowerable(op) => {
                let wa = ((op >> 20) & 0x1f) as u8;
                let wb = (((op >> 10) & 0xf) as u8) | 0x20;
                self.store_ppc(addr);
                let a = self.get_treg(wa);
                let b = self.get_treg(wb);
                self.emit_alu2(op);
                self.set_treg(wa, a);
                self.set_treg(wb, b);
                true
            }
            // Class 1, internal register -> register transfer.
            Class::Alu2Trans1 if op & (1 << 26) == 0 && alu2_lowerable(op) => {
                let sr = ((op >> 19) & 0x7f) as u8;
                let dr = ((op >> 12) & 0x7f) as u8;
                if !treg_lowerable(sr) || !treg_lowerable(dr) {
                    return false;
                }
                self.store_ppc(addr);
                let res = self.get_treg(sr);
                self.emit_alu2(op);
                self.set_treg(dr, res);
                true
            }
            // Class 4, sda == sdb == 0: register -> ALU1 -> register, twice.
            Class::Alu1Trans2
                if (op >> 38) & 3 == 0 && (op >> 18) & 3 == 0 && alu1_lowerable(op) =>
            {
                let wa = ((op >> 33) & 0x1f) as u8;
                let wa_d = ((op >> 28) & 0x1f) as u8;
                let wb = (((op >> 13) & 0x1f) as u8) | 0x20;
                let wb_d = (((op >> 8) & 0x1f) as u8) | 0x20;
                // These 5-bit fields | 0x20 can reach group 6 (FIFO/PR, all
                // side effects); keep those on the trampoline.
                if !treg_lowerable(wb) || !treg_lowerable(wb_d) {
                    return false;
                }
                self.store_ppc(addr);
                let a = self.get_treg(wa);
                let b = self.get_treg(wb);
                self.emit_alu1(op);
                self.set_treg(wa_d, a);
                self.set_treg(wb_d, b);
                true
            }
            // Class 5, internal register -> register transfer.
            Class::Alu1Trans1 if op & (1 << 38) == 0 && alu1_lowerable(op) => {
                let sr = ((op >> 31) & 0x7f) as u8;
                let dr = ((op >> 24) & 0x7f) as u8;
                if !treg_lowerable(sr) || !treg_lowerable(dr) {
                    return false;
                }
                self.store_ppc(addr);
                let res = self.get_treg(sr);
                self.emit_alu1(op);
                self.set_treg(dr, res);
                true
            }
            // Class 7: a 32-bit immediate to a register.
            Class::Trans1 => {
                let dr = ((op >> 19) & 0x7f) as u8;
                if !treg_lowerable(dr) {
                    return false;
                }
                let imm = ((op >> 27) & 0xffff_ffff) as u32;
                self.store_ppc(addr);
                let v = self.iconst32(imm);
                self.set_treg(dr, v);
                true
            }
            _ => false,
        }
    }

    fn call_exec(&mut self, addr: u32, op: u64) {
        let curpc = self.iconst32(addr);
        let opc = self.fb.ins().iconst(types::I64, op as i64);
        self.fb
            .ins()
            .call(self.fr.exec, &[self.cpu, self.busp, self.vt, curpc, opc]);
    }

    /// Block exit: leave `pc` at the instruction after the last executed one
    /// (for a stall exit that is also where the interpreter leaves it), charge
    /// `executed` instructions to `icount`/`insns`, and return.
    fn exit(&mut self, next_pc: u32, executed: u32) {
        let pc = self.iconst32(next_pc);
        self.fb.ins().store(flags(), pc, self.cpu, OFF_PC);
        let ic = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_ICOUNT);
        let ic = self.fb.ins().iadd_imm(ic, -(executed as i64));
        self.fb.ins().store(flags(), ic, self.cpu, OFF_ICOUNT);
        let ins = self.fb.ins().load(types::I64, flags(), self.cpu, OFF_INSNS);
        let ins = self.fb.ins().iadd_imm(ins, executed as i64);
        self.fb.ins().store(flags(), ins, self.cpu, OFF_INSNS);
        let zero = self.iconst32(0);
        self.fb.ins().return_(&[zero]);
    }
}

// --- opcode field accessors, identical to the interpreter's in `alu.rs` ------

#[inline]
fn aop(op: u64) -> u8 {
    ((op >> 56) & 0x1f) as u8
}
#[inline]
fn ai1(op: u64) -> u8 {
    ((op >> 52) & 0x0f) as u8
}
#[inline]
fn ai2(op: u64) -> u8 {
    ((op >> 47) & 0x1f) as u8
}
#[inline]
fn ao(op: u64) -> u8 {
    ((op >> 42) & 0x1f) as u8
}
#[inline]
fn mop(op: u64) -> u8 {
    ((op >> 41) & 0x01) as u8
}
#[inline]
fn mi1(op: u64) -> u8 {
    ((op >> 37) & 0x0f) as u8
}
#[inline]
fn mi2(op: u64) -> u8 {
    ((op >> 32) & 0x1f) as u8
}
#[inline]
fn mo(op: u64) -> u8 {
    ((op >> 27) & 0x1f) as u8
}

/// The constant-table encodings, mirroring `alu.rs` (`const_float` /
/// `const_int`) so the native sources are the same bits the interpreter reads.
fn const_float_bits(which: u8) -> u32 {
    const T: [f32; 8] = [-1.0, 0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 5.0];
    T[(which & 7) as usize].to_bits()
}
fn const_int_bits(which: u8) -> u32 {
    match which & 7 {
        0 => 0,
        1 => 1,
        _ => 0xffff_ffff,
    }
}

// --- lowering predicates -----------------------------------------------------

/// Transfer-slot register codes the native lowering can touch: the 0x40 bit
/// clear (a register, not a memory access with its EA post-actions and
/// external-bus reach) and not group 6 (FIFO/PR/ports, all side effects).
/// Groups 7+ are fine: they read as 0 and drop writes, like the interpreter.
fn treg_lowerable(which: u8) -> bool {
    which & 0x40 == 0 && (which >> 3) != 6
}

/// The PR-ring group. Reading it post-modifies `prp`, so native code (which
/// has no such side effect) must keep every source off it.
fn off_pr(which: u8) -> bool {
    (which >> 3) != 2
}

/// The ALU opcodes `Emit::emit_aluop` handles natively. FEA/FES, FRCP/FRSQ,
/// FLOG and CFIB stay on the trampoline (transcendental calls and rarer flag
/// semantics are not worth inlining).
fn aluop_lowered(a: u8) -> bool {
    matches!(a, 0x00..=0x07 | 0x0d | 0x0e | 0x10..=0x1f)
}

/// Whether the dual ALU slot (classes 0/1) can issue natively: a lowered ALU
/// op, the multiplier (FMUL/IMUL are both lowered), and no PR-ring source.
/// The 4-bit source fields reach only the register banks; the 5-bit ones can
/// name the ring, and `ai2` is only read when the op has a second source.
fn alu2_lowerable(op: u64) -> bool {
    aluop_lowered(aop(op))
        && (!Mb86235::alu_has_second_src(aop(op)) || off_pr(ai2(op)))
        && off_pr(mi2(op))
}

/// Whether the single ALU slot (classes 4/5) can issue natively: bit 41 picks
/// the ALU form (same gates as the dual slot) or the multiplier form, where
/// `ai2` doubles as `mi2` and is always read.
fn alu1_lowerable(op: u64) -> bool {
    if op & (1 << 41) != 0 {
        aluop_lowered(aop(op)) && (!Mb86235::alu_has_second_src(aop(op)) || off_pr(ai2(op)))
    } else {
        off_pr(ai2(op))
    }
}

/// Instructions after which the sequential-fetch assumption no longer holds:
/// anything that can redirect control flow, and REP, which holds the PC at
/// runtime. Non-branch control operations (NOP/SETL/CLRF/PUSH/POP/MOD writes)
/// stay inside the block.
fn ends_block(op: u64) -> bool {
    if !matches!(classify(op), Class::Control) {
        return false;
    }
    let cop = ((op >> 22) & 0x1f) as u32;
    matches!(cop, 0x01 | 0x10..=0x15 | 0x18..=0x1b)
}

/// The control-class NOP with a one-operand ALU NOP slot: bit 63 set
/// (`do_alu1`), bit 41 set (ALU, not the multiplier), `aop == 0x07` (NOP),
/// and the unused second source kept off the PR ring (reading it would
/// post-increment `prp`). Completely free.
fn is_free_nop(op: u64) -> bool {
    (op >> 61) & 7 == 6
        && (op >> 22) & 0x1f == 0
        && op & (1 << 41) != 0
        && (op >> 56) & 0x1f == 0x07
        && ((op >> 47) & 0x1f) < 16
}

/// The JIT `execute` driver: mirrors the interpreter's loop, but runs
/// compiled blocks whenever the fetch address is simply `pc` -- no stall
/// retry in flight, no delay slot, no REP holding the PC. Those three states
/// go through the interpreter's `step`, after which the sequential
/// assumption holds again.
pub fn execute<B: Mb86235Bus>(cpu: &mut Mb86235, bus: &mut B, cycles: i32) {
    cpu.icount = cycles;
    if cpu.jit.0.is_none() {
        cpu.jit.0 = Some(Jit::new());
    }
    // Take the cache out so the CPU struct can be borrowed freely; it goes
    // back before returning.
    let mut jit = match cpu.jit.0.take().and_then(|b| b.downcast::<Jit>().ok()) {
        Some(j) => j,
        None => Jit::new(),
    };
    let vt = vtable::<B>();
    let vt = &vt as *const Mb86235Vtable;
    let busp = bus as *mut B as *mut c_void;
    while cpu.icount > 0 {
        if !cpu.stalled && !cpu.delay_slot && cpu.st & flag::RP == 0 {
            let f = jit.block_for(cpu, cpu.pc);
            unsafe {
                f(cpu, busp, vt);
            }
        } else {
            cpu.step(bus);
        }
    }
    cpu.jit.0 = Some(jit);
}
