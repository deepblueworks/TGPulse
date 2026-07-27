//! Cranelift block dynarec for the ADSP-21062, with the interpreter as the
//! exact fallback for everything not lowered natively. Mirrors the i960 JIT's
//! architecture (`crates/i960/src/cpu/jit.rs`):
//!
//! * A **block** is a run of guest instructions starting at a fetch address
//!   with a sequential pipeline (`faddr == daddr+1`, `nfaddr == daddr+2`).
//!   Compilation stops at the first instruction that can redirect control
//!   flow (jumps/calls/returns, IDLE), at a non-internal fetch address, or at
//!   a 32-instruction cap. The driver runs the interpreter's `step` for the
//!   two delay-slot instructions behind a delayed branch, where the pipeline
//!   is not sequential.
//! * Blocks are cached by entry address. All guest state lives in the `Sharc`
//!   struct in memory (reached through `offset_of!` offsets), so a block can
//!   call back into Rust at any point with the architected state consistent.
//!   `icount`/`insns` are charged at each exit by a compile-time constant.
//! * The SHARC's per-instruction pipeline advance is compile-time constant
//!   inside a block, so `pc/daddr/faddr/nfaddr` are only stored before helper
//!   calls and at exits.
//! * Every instruction with side effects goes through a trampoline into the
//!   interpreter's own `dispatch` (`t_dispatch`) -- "not lowered natively"
//!   never means "behaves differently". Natively lowered so far: `NOP` and
//!   compute-slot-less `Compute`, which are free. The win over the
//!   interpreter loop is the hoisted fetch/decode (compile-time), the
//!   vectorized cycle accounting, and the FIFO-flag refresh emitted only
//!   before instructions that can actually observe the flags.
//! * The hardware `DO UNTIL` loop close is checked inline per instruction
//!   (`laddr_addr`/`LSEM` in memory); a hit calls `t_loop_check`, which runs
//!   the interpreter's `handle_loop` verbatim and ends the block when the PC
//!   was redirected.
//! * Interrupts are sampled between blocks (the interpreter samples between
//!   instructions); `check_interrupts` runs unchanged.
//! * Uploaded/self-modifying microcode: every internal PM write bumps a
//!   per-page epoch (`Sharc::code_epochs`); a cached block records the epochs
//!   of the pages it spans and is recompiled when one moves.
//!
//! The bus is type-erased through a vtable (`SharcVtable`) rather than by
//! monomorphizing the cache over `B: SharcBus`, because one of the two bus
//! implementations (the coprocessor worker's `BatchBus<'_>`) borrows and is
//! not `'static`. Lowering decisions follow the same basic-block contract as
//! MAME's BSD-3 SHARC DRC front-end (`src/devices/cpu/sharc/sharcdrc.cpp`):
//! end blocks at control flow, fall back to C for anything not lowered.

// cranelift 0.134 renamed the `_imm` builders to `_imm_s`/`_imm_u`; the old
// names keep their sign-extending behaviour, which is what the exits want.
#![allow(deprecated)]

use std::ffi::c_void;
use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, AbiParam, FuncRef, InstBuilder, MemFlagsData, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use crate::consts::LSEM;
use crate::ops::{Op, decode};
use crate::state::Sharc;
use crate::SharcBus;

/// Instructions per block cap. Bounds IRQ sampling latency and the quantum
/// overshoot (the interpreter stops mid-block at `icount == 0`; the JIT
/// charges at block exits).
const MAX_BLOCK_INSNS: usize = 32;

/// Dev-only override for the block length cap, used by the dual-run test:
/// single-instruction blocks make the JIT stop at exactly the cycle counts
/// the interpreter does, so the two can be diffed in lockstep. 0 = no
/// override.
pub static BLOCK_CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Compiled block entry: guest CPU, type-erased bus, bus vtable. Returns 0.
type BlockFn = unsafe extern "C" fn(*mut Sharc, *mut c_void, *const SharcVtable) -> i32;

const OFF_PC: i32 = offset_of!(Sharc, pc) as i32;
const OFF_DADDR: i32 = offset_of!(Sharc, daddr) as i32;
const OFF_FADDR: i32 = offset_of!(Sharc, faddr) as i32;
const OFF_NFADDR: i32 = offset_of!(Sharc, nfaddr) as i32;
const OFF_ICOUNT: i32 = offset_of!(Sharc, icount) as i32;
const OFF_INSNS: i32 = offset_of!(Sharc, insns) as i32;
const OFF_LADDR: i32 = offset_of!(Sharc, laddr_addr) as i32;
const OFF_STKY: i32 = offset_of!(Sharc, stky) as i32;

// --- bus vtable (type erasure over B: SharcBus) ------------------------------

/// Function pointers for one `SharcBus` implementation, so compiled blocks
/// and their Rust helpers are bus-type-agnostic.
pub struct SharcVtable {
    dm_ext_read: unsafe extern "C" fn(*mut c_void, u32) -> u32,
    dm_ext_write: unsafe extern "C" fn(*mut c_void, u32, u32),
    pm_ext_read: unsafe extern "C" fn(*mut c_void, u32) -> u64,
    pm_ext_write: unsafe extern "C" fn(*mut c_void, u32, u64),
    fifo_in_empty: unsafe extern "C" fn(*mut c_void) -> u32,
    fifo_out_full: unsafe extern "C" fn(*mut c_void) -> u32,
}

unsafe extern "C" fn vt_dm_ext_read<B: SharcBus>(p: *mut c_void, a: u32) -> u32 {
    unsafe { (*(p as *mut B)).dm_ext_read(a) }
}
unsafe extern "C" fn vt_dm_ext_write<B: SharcBus>(p: *mut c_void, a: u32, d: u32) {
    unsafe { (*(p as *mut B)).dm_ext_write(a, d) }
}
unsafe extern "C" fn vt_pm_ext_read<B: SharcBus>(p: *mut c_void, a: u32) -> u64 {
    unsafe { (*(p as *mut B)).pm_ext_read(a) }
}
unsafe extern "C" fn vt_pm_ext_write<B: SharcBus>(p: *mut c_void, a: u32, d: u64) {
    unsafe { (*(p as *mut B)).pm_ext_write(a, d) }
}
unsafe extern "C" fn vt_fifo_in_empty<B: SharcBus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_in_empty() as u32 }
}
unsafe extern "C" fn vt_fifo_out_full<B: SharcBus>(p: *mut c_void) -> u32 {
    unsafe { (*(p as *mut B)).fifo_out_full() as u32 }
}

/// Builds the vtable for one bus type (six function pointers; assembled on
/// the caller's stack per `execute` batch).
fn vtable<B: SharcBus>() -> SharcVtable {
    SharcVtable {
        dm_ext_read: vt_dm_ext_read::<B>,
        dm_ext_write: vt_dm_ext_write::<B>,
        pm_ext_read: vt_pm_ext_read::<B>,
        pm_ext_write: vt_pm_ext_write::<B>,
        fifo_in_empty: vt_fifo_in_empty::<B>,
        fifo_out_full: vt_fifo_out_full::<B>,
    }
}

/// A `SharcBus` that forwards through the vtable; the helpers build one on
/// the stack and then run the interpreter's own code verbatim.
struct BusPtr {
    bus: *mut c_void,
    vt: *const SharcVtable,
}

impl SharcBus for BusPtr {
    fn dm_ext_read(&mut self, addr: u32) -> u32 {
        unsafe { ((*self.vt).dm_ext_read)(self.bus, addr) }
    }
    fn dm_ext_write(&mut self, addr: u32, data: u32) {
        unsafe { ((*self.vt).dm_ext_write)(self.bus, addr, data) }
    }
    fn pm_ext_read(&mut self, addr: u32) -> u64 {
        unsafe { ((*self.vt).pm_ext_read)(self.bus, addr) }
    }
    fn pm_ext_write(&mut self, addr: u32, data: u64) {
        unsafe { ((*self.vt).pm_ext_write)(self.bus, addr, data) }
    }
    fn fifo_in_empty(&self) -> bool {
        unsafe { ((*self.vt).fifo_in_empty)(self.bus) != 0 }
    }
    fn fifo_out_full(&self) -> bool {
        unsafe { ((*self.vt).fifo_out_full)(self.bus) != 0 }
    }
}

// --- Rust callbacks reachable from compiled code -----------------------------

/// The fallback every non-trivial instruction goes through: the interpreter's
/// exact `dispatch` for the already-decoded opcode.
unsafe extern "C" fn t_dispatch(
    cpu: *mut Sharc,
    bus: *mut c_void,
    vt: *const SharcVtable,
    opcode: i64,
) {
    let c = unsafe { &mut *cpu };
    let mut b = BusPtr { bus, vt };
    let opcode = opcode as u64 & 0xffff_ffff_ffff;
    c.opcode = opcode;
    let op = decode(opcode);
    c.dispatch(&mut b, op);
}

/// The interpreter's per-instruction FIFO-flag refresh, emitted only before
/// instructions that can observe FLAG0/1 (flag conditions, ASTAT reads).
unsafe extern "C" fn t_flags(cpu: *mut Sharc, bus: *mut c_void, vt: *const SharcVtable) {
    let c = unsafe { &mut *cpu };
    let b = BusPtr { bus, vt };
    c.flag[0] = b.fifo_in_empty() as u32;
    c.flag[1] = b.fifo_out_full() as u32;
}

/// Hardware-loop close for the instruction at `expected - 1`: refreshes the
/// flags (the interpreter refreshes before the loop condition is tested),
/// runs `handle_loop`, and reports whether the PC was redirected.
unsafe extern "C" fn t_loop_check(
    cpu: *mut Sharc,
    bus: *mut c_void,
    vt: *const SharcVtable,
    expected: u32,
) -> i32 {
    let c = unsafe { &mut *cpu };
    t_flags(cpu, bus, vt);
    c.handle_loop();
    (c.daddr != expected) as i32
}

struct HelperIds {
    dispatch: FuncId,
    flags: FuncId,
    loop_check: FuncId,
}

struct FuncRefs {
    dispatch: FuncRef,
    flags: FuncRef,
    loop_check: FuncRef,
}

#[derive(Clone, Copy)]
struct CompiledBlock {
    f: BlockFn,
    /// Entry address this block was compiled for (direct-mapped cache tag).
    tag: u32,
    // PM pages (addr >> 13 of the internal range) the block spans, and their
    // code epochs at compile time. A write to either page recompiles.
    p0: usize,
    p1: usize,
    e0: u64,
    e1: u64,
}

/// Block cache size (direct-mapped L1 over a HashMap backing store, same
/// shape as the i960 cache).
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
    /// Diagnostics: blocks compiled, and recompiles caused by code writes.
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
        builder.symbol("sharc_dispatch", t_dispatch as *const u8);
        builder.symbol("sharc_flags", t_flags as *const u8);
        builder.symbol("sharc_loop_check", t_loop_check as *const u8);
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
        let dispatch = sig(&[ptr, ptr, ptr, types::I64], &[], &mut module);
        let flags_s = sig(&[ptr, ptr, ptr], &[], &mut module);
        let lc = sig(&[ptr, ptr, ptr, types::I32], &[types::I32], &mut module);
        let block_sig = sig(&[ptr, ptr, ptr], &[types::I32], &mut module);

        let helpers = HelperIds {
            dispatch: module
                .declare_function("sharc_dispatch", Linkage::Import, &dispatch)
                .expect("declare"),
            flags: module
                .declare_function("sharc_flags", Linkage::Import, &flags_s)
                .expect("declare"),
            loop_check: module
                .declare_function("sharc_loop_check", Linkage::Import, &lc)
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

    /// The block for `entry`, compiling (or recompiling after a code write)
    /// as needed. `None` when the entry address is not internal PM -- the
    /// driver runs the interpreter's `step` for it.
    fn block_for(&mut self, cpu: &Sharc, entry: u32) -> Option<BlockFn> {
        let slot = (entry as usize) & (CACHE_SIZE - 1);
        if let Some(b) = self.l1[slot] {
            if b.tag == entry && self.epochs_match(cpu, &b) {
                return Some(b.f);
            }
        }
        match self.cache.get(&entry) {
            Some(b) if self.epochs_match(cpu, b) => {
                let b = *b;
                self.l1[slot] = Some(b);
                Some(b.f)
            }
            _ => {
                if self.cache.contains_key(&entry) {
                    self.recompiles += 1;
                }
                self.compiles += 1;
                let b = self.compile(cpu, entry)?;
                let f = b.f;
                self.cache.insert(entry, b);
                self.l1[slot] = Some(b);
                Some(f)
            }
        }
    }

    fn epochs_match(&self, cpu: &Sharc, b: &CompiledBlock) -> bool {
        cpu.code_epochs[b.p0] == b.e0 && (b.p1 == b.p0 || cpu.code_epochs[b.p1] == b.e1)
    }

    fn compile(&mut self, cpu: &Sharc, entry: u32) -> Option<CompiledBlock> {
        // The first word must be internal PM, or this address can never be
        // compiled (external fetches may have bus side effects).
        pm_word(cpu, entry)?;

        self.counter += 1;
        let name = format!("sharc_blk_{entry:06x}_{}", self.counter);
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

        let mut last = entry;
        let mut n = 0u32;
        {
            let mut fb = FunctionBuilder::new(&mut ctx.func, fb_ctx);
            let fr = FuncRefs {
                dispatch: module.declare_func_in_func(helpers.dispatch, fb.func),
                flags: module.declare_func_in_func(helpers.flags, fb.func),
                loop_check: module.declare_func_in_func(helpers.loop_check, fb.func),
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
                let opcode = match pm_word(cpu, addr) {
                    Some(w) => w,
                    None => {
                        // External fetch ahead: end the block before it.
                        e.exit_sequential(last, n);
                        break;
                    }
                };
                let op = decode(opcode);
                let end = e.emit_insn(addr, opcode, op, n);
                n += 1;
                last = addr;
                if end {
                    break;
                }
                if n as usize >= cap {
                    e.exit_sequential(last, n);
                    break;
                }
                addr = addr.wrapping_add(1);
            }
            e.fb.finalize(frontend_cfg);
        }

        module
            .define_function(fid, &mut ctx)
            .expect("sharc jit: define_function");
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize");
        let ptr = module.get_finalized_function(fid);

        let p0 = page(entry);
        let p1 = page(last);
        Some(CompiledBlock {
            f: unsafe { std::mem::transmute::<*const u8, BlockFn>(ptr) },
            tag: entry,
            p0,
            p1,
            e0: cpu.code_epochs[p0],
            e1: cpu.code_epochs[p1],
        })
    }
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// The internal-PM word at `addr`, or `None` for external space.
fn pm_word(cpu: &Sharc, addr: u32) -> Option<u64> {
    let i = Sharc::internal(addr)?;
    Some(cpu.pm[i] & 0xffff_ffff_ffff)
}

/// Code-epoch page of an internal PM address (0x2000-word pages).
fn page(addr: u32) -> usize {
    ((addr.wrapping_sub(0x20000) >> 13) as usize) & 0xf
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

    /// Emits one instruction: the inline hardware-loop guard, the FIFO-flag
    /// refresh when the instruction can observe the flag pins, and the
    /// dispatch-helper call for anything with architectural effect. Returns
    /// true when the instruction ends the block (the exit is already
    /// emitted).
    ///
    /// The loop close mirrors the interpreter exactly: the bottom-of-loop
    /// instruction *executes* on the iteration that branches back, with the
    /// PC pipeline already redirected (`handle_loop` runs before dispatch).
    /// The redirect path therefore skips `store_pipeline` -- `change_pc` has
    /// already left the correct state in memory -- and always ends the block.
    fn emit_insn(&mut self, addr: u32, opcode: u64, op: Op, executed: u32) -> bool {
        // NOP and a compute slot of zero have no architectural effect.
        let free = op == Op::Nop || (op == Op::Compute && opcode & 0x7fffff == 0);
        let end = ends_block(op);
        let want_flags = !free && reads_flags(op, opcode);

        let la = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_LADDR);
        let hit = self.fb.ins().icmp_imm(IntCC::Equal, la, addr as i64);
        let stky = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_STKY);
        let masked = self.fb.ins().band_imm(stky, LSEM as i64);
        let armed = self.fb.ins().icmp_imm(IntCC::Equal, masked, 0);
        let guard = self.fb.ins().band(hit, armed);
        let slow_bb = self.fb.create_block();
        let normal_bb = self.fb.create_block();
        self.fb.ins().brif(guard, slow_bb, &[], normal_bb, &[]);

        // Normal path: no loop closes here.
        self.fb.switch_to_block(normal_bb);
        if want_flags {
            self.call_flags();
        }
        if !free {
            self.store_pipeline(addr);
            self.call_dispatch(opcode);
        }
        let cont_bb = if end {
            self.sync_exit(executed + 1);
            None
        } else {
            let bb = self.fb.create_block();
            self.fb.ins().jump(bb, &[]);
            Some(bb)
        };
        self.fb.seal_block(normal_bb);

        // Loop-close path: handle_loop via the helper, then the instruction
        // still executes (see above). The pipeline consts are stored first:
        // the helper detects the redirect by comparing `daddr` against the
        // sequential next, which is only meaningful with them in place.
        self.fb.switch_to_block(slow_bb);
        self.store_pipeline(addr);
        let expected = self.iconst32(addr.wrapping_add(1));
        let call = self.fb.ins().call(
            self.fr.loop_check,
            &[self.cpu, self.busp, self.vt, expected],
        );
        let redirected = self.fb.inst_results(call)[0];
        let r = self.fb.ins().icmp_imm(IntCC::NotEqual, redirected, 0);
        let redir_bb = self.fb.create_block();
        let term_bb = self.fb.create_block();
        self.fb.ins().brif(r, redir_bb, &[], term_bb, &[]);

        // Loop terminated (stacks popped): the sequential pipeline continues
        // (already stored above).
        self.fb.switch_to_block(term_bb);
        if !free {
            self.call_dispatch(opcode);
        }
        match cont_bb {
            Some(bb) => {
                self.fb.ins().jump(bb, &[]);
            }
            None => self.sync_exit(executed + 1),
        }
        self.fb.seal_block(term_bb);

        // Loop repeated: change_pc already wrote the pipeline; run the
        // instruction with that state and end the block.
        self.fb.switch_to_block(redir_bb);
        if !free {
            self.call_dispatch(opcode);
        }
        self.sync_exit(executed + 1);
        self.fb.seal_block(redir_bb);
        self.fb.seal_block(slow_bb);

        if let Some(bb) = cont_bb {
            self.fb.switch_to_block(bb);
            self.fb.seal_block(bb);
        }
        end
    }

    fn call_flags(&mut self) {
        self.fb.ins().call(self.fr.flags, &[self.cpu, self.busp, self.vt]);
    }

    /// The per-instruction pipeline state, constant inside a block; stored
    /// before every helper call and at sequential exits so the architected
    /// state in memory always matches the interpreter's.
    fn store_pipeline(&mut self, addr: u32) {
        let pc = self.iconst32(addr);
        self.fb.ins().store(flags(), pc, self.cpu, OFF_PC);
        let d = self.iconst32(addr.wrapping_add(1));
        self.fb.ins().store(flags(), d, self.cpu, OFF_DADDR);
        let f = self.iconst32(addr.wrapping_add(2));
        self.fb.ins().store(flags(), f, self.cpu, OFF_FADDR);
        let nf = self.iconst32(addr.wrapping_add(3));
        self.fb.ins().store(flags(), nf, self.cpu, OFF_NFADDR);
    }

    fn call_dispatch(&mut self, opcode: u64) {
        let op = self.fb.ins().iconst(types::I64, opcode as i64);
        self.fb
            .ins()
            .call(self.fr.dispatch, &[self.cpu, self.busp, self.vt, op]);
    }

    /// Charges `executed` instructions to `icount`/`insns` and returns.
    fn sync_exit(&mut self, executed: u32) {
        let ic = self.fb.ins().load(types::I32, flags(), self.cpu, OFF_ICOUNT);
        let ic = self.fb.ins().iadd_imm(ic, -(executed as i64));
        self.fb.ins().store(flags(), ic, self.cpu, OFF_ICOUNT);
        let ins = self.fb.ins().load(types::I64, flags(), self.cpu, OFF_INSNS);
        let ins = self.fb.ins().iadd_imm(ins, executed as i64);
        self.fb.ins().store(flags(), ins, self.cpu, OFF_INSNS);
        let zero = self.iconst32(0);
        self.fb.ins().return_(&[zero]);
    }

    /// Exit after the instruction at `last`, with the sequential pipeline
    /// for the following instruction left in memory.
    fn exit_sequential(&mut self, last: u32, executed: u32) {
        self.store_pipeline(last);
        self.sync_exit(executed);
    }
}

/// `cond` values that read the FLAG pins.
fn cond_reads_flags(c: u32) -> bool {
    matches!(c, 0x09..=0x0c | 0x19..=0x1c)
}

/// Whether executing this instruction can observe `flag[0..2]` -- directly
/// through a condition code, or through an ASTAT read (`astat_with_flags`
/// merges the pins). Compiled blocks refresh the flags only before these.
fn reads_flags(op: Op, opcode: u64) -> bool {
    use Op::*;
    match op {
        Compute | DirectJump | DirectCall | RelativeJump | RelativeCall | IndirectJump
        | IndirectCall | RelativeJumpCompute | RelativeCallCompute
        | IndirectJumpComputeDregDm | RelativeJumpComputeDregDm | Rts | Rti | ImmShift
        | ComputeDmToDregImmmod | ComputeDregToDmImmmod | ComputePmToDregImmmod
        | ComputeDregToPmImmmod | ComputeUregDmpmPremod | ComputeUregDmpmPostmod => {
            cond_reads_flags(((opcode >> 33) & 0x1f) as u32)
        }
        ComputeUregToUreg => {
            cond_reads_flags(((opcode >> 31) & 0x1f) as u32)
                || ((opcode >> 36) & 0xff) as usize == 0x7c
        }
        DmToUregDirect | PmToUregDirect | DmToUregIndirect | PmToUregIndirect
        | DoUntilCounterUreg => ((opcode >> 32) & 0xff) as usize == 0x7c,
        SysregBitop => ((opcode >> 32) & 0xf) as usize == 0xc,
        _ => false,
    }
}

/// Instructions that can redirect the fetch pipeline (or park the core):
/// the block ends after them. `DO UNTIL` only arms a loop and falls through,
/// so it is *not* here; the inline loop guard handles the close.
fn ends_block(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        DirectJump
            | DirectCall
            | RelativeJump
            | RelativeCall
            | IndirectJump
            | IndirectCall
            | RelativeJumpCompute
            | RelativeCallCompute
            | IndirectJumpComputeDregDm
            | RelativeJumpComputeDregDm
            | Rts
            | Rti
            | Idle
    )
}

/// The JIT `execute` driver: mirrors the interpreter's loop, but runs
/// compiled blocks whenever the fetch pipeline is sequential. The two
/// delay-slot instructions behind a delayed branch (where `nfaddr` points
/// at the branch target) go through `step`, after which the pipeline is
/// sequential again.
pub fn execute<B: SharcBus>(cpu: &mut Sharc, bus: &mut B, cycles: i32) {
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
    let vt = &vt as *const SharcVtable;
    let busp = bus as *mut B as *mut c_void;
    while cpu.icount > 0 && !cpu.idle {
        if cpu.irq_pending != 0 {
            cpu.check_interrupts();
        }
        let sequential =
            cpu.faddr == cpu.daddr.wrapping_add(1) && cpu.nfaddr == cpu.daddr.wrapping_add(2);
        if sequential {
            match jit.block_for(cpu, cpu.daddr) {
                Some(f) => unsafe {
                    f(cpu, busp, vt);
                },
                None => cpu.step(bus),
            }
        } else {
            cpu.step(bus);
        }
    }
    cpu.jit.0 = Some(jit);
}
