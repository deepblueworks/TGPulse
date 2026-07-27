# Performance plan

Current state: every processor core (i960, V60, SHARC ADSP-21062, MB86233,
MB86235, 68000) is an interpreter with match-based dispatch, and all chips
run sequentially on one thread. On desktop this is enough; on Android phones
games run below full speed.

This document records what was researched before changing anything, and the
order of work that follows from it.

## What already exists elsewhere

Checked before writing any code, to avoid rebuilding something available:

- **No dynarec exists for the i960, V60 or MB86233**, in any language. The
  only i960 recompiler ever written is the closed-source x86 one in ElSemi's
  Model 2 Emulator (last release 2014). These three front-ends must be
  written regardless of approach.
- **MAME has DRC front-ends for the SHARC and MB86235** (`src/devices/cpu/
  sharc/sharcdrc.cpp`, `src/devices/cpu/mb86235/mb86235drc.cpp`) and an ARM64
  DRC back-end (`drcbearm64.cpp`, built on asmjit). All BSD-3-Clause. The
  code is tied to MAME's device framework and cannot be imported, but it is a
  usable blueprint for lowering these instruction sets to IR.
- **68000 has several fast cores** (Amiberry and Emu68 have AArch64 JITs;
  Cyclone 68000 is ARM32-only). Not needed: the 68k runs sound, is cheap to
  interpret, and stays on the main thread.

## JIT back-end: Cranelift

Chosen back-end for any dynarec work: `cranelift-jit`, from the Bytecode
Alliance (Wasmtime's compiler).

- Pure Rust; AArch64 is a tier-1 target; Apache-2.0 with LLVM exception.
- Compile latency is designed for JIT workloads, unlike LLVM.
- Already used this way by emulators: Gecko (GameCube/Wii, Rust) runs
  Cranelift JITs for PowerPC, the GameCube DSP and the vertex decoder;
  RustEE (PS2, Rust) offers interpreter and Cranelift backends. PowerPC is a
  32-bit load/store RISC, structurally close to the i960 and V60, so Gecko
  is the reference implementation to follow.

Alternatives considered and rejected: LLVM via inkwell (compile latency,
linking LLVM for Android is painful), libgccjit (runtime shared-library
dependency, GPL), QEMU libtcg (no maintained upstream library), dynarmic
(recompiles guest ARM only), GNU lightning (GPLv3), dynasm-rs (has an
AArch64 backend but MPL-2.0 and little maintenance), asmjit (capable, but
C++ FFI and a full code generator to write; kept as fallback).

On Android, JIT is permitted (Dolphin, PPSSPP, Flycast ship dynarecs); W^X
applies, which Cranelift's memory API handles.

## Threading the coprocessors

The coprocessors are separate chips communicating through FIFO mailboxes,
and the main CPU already stalls when an output FIFO is empty. That stall is
the same backpressure model as PCSX2's MTVU, so the proven pattern applies
directly:

- One long-lived worker thread per SHARC/TGP. Each hardware FIFO direction
  becomes a bounded SPSC ring buffer (cache-padded head/tail atomics, or
  `crossbeam-channel` bounded). The existing stall logic becomes the
  blocking point; no new synchronization semantics.
- Batch DSP work: run thousands of cycles per wake-up. PCSX2's Time Crisis 2
  regressed under MTVU because of fine-grained synchronization; batching is
  the tuning knob.
- The sound CPU stays on the main thread (Flycast's approach: it is
  audio-rate-coupled and cheap).
- Savestates use Supermodel's stop-the-world method: pause all worker
  threads at a frame boundary, serialize every core plus the FIFO contents,
  resume. Dolphin's DSP savestates show the rule: in-flight mailbox/FIFO
  contents are part of the serialized state.
- Keep a `multithreaded = off` setting that runs the current cooperative
  loop. It is the determinism reference for A/B testing, as Supermodel and
  PCSX2 do with their own toggles.
- Pin the main CPU thread to a big core (`sched_setaffinity`) on Android's
  big.LITTLE schedulers.

Expected gain: 1.5-2.5x in DSP-heavy scenes on an 8-core phone.

## Order of work

1. **Profile.** Confirm the build is CPU-bound and measure which cores cost
   the most. Cheap, and it decides how far the later steps need to go.
2. **Thread the coprocessors** as described above. Moderate effort, no new
   correctness surface beyond synchronization, helps every game.
3. **Cranelift dynarec for the i960.** The i960 is the bottleneck CPU and a
   simple RISC, so it goes first. Gecko is the template. Expect 3-10x on
   main-CPU-bound workloads.
4. **SHARC/TGP dynarecs** afterwards, using MAME's BSD-3 DRC front-ends as
   the lowering reference.
5. The 68k stays interpreted.

### Status: step 3 landed (feat/gottagofast)

The i960 has a Cranelift block dynarec (`crates/i960/src/cpu/jit.rs`,
feature `jit`, default on; runtime switch `Config::i960_jit` /
`--i960-jit on|off`). Basic blocks are compiled and cached by entry IP; the
integer/logic/move, compare, branch, single-word load/store and `lda`/`bal`
subset is lowered natively, everything else (FPU, call/ret, burst transfers,
div/rem, system ops) calls the interpreter's per-instruction path verbatim,
so nothing behaves differently -- only faster or identically. All bus
accesses call back into Rust (MMIO side effects); cycle accounting,
`LIVE_ICOUNT`, stall rewind and fault behaviour match the interpreter, with
IRQ sampling and internal-timer ticks at basic-block granularity. Code
writes invalidate per 4 KiB page (`Bus::code_epoch`). Breakpoints and the
trace ring force the interpreter while active. The test suite runs through
the JIT by default (`I960_JIT=0` for the interpreter, `I960_DUALRUN=1` for a
lockstep interpreter/JIT differential).

First-pass results for `run 600` headless (vs interpreter): vf2 ~12%
faster, vstriker ~2%, daytona parity. Further gains need block chaining
(today every block exit re-enters the dispatcher) and lowering call/ret --
call-heavy code still pays the fallback. Compile latency is tuned with
`opt_level=none`; block-compile cost is ~2% of frame time at boot and
negligible steady-state.

### Status: step 4 landed for the SHARC (feat/gottagofast)

The ADSP-21062 has a Cranelift block dynarec (`crates/sharc/src/jit.rs`,
feature `jit` on the sharc crate, default on; runtime switch
`Config::sharc_jit` / `--sharc-jit on|off`, mirrored at savestate restore).
Blocks are cached by fetch address and entered only when the fetch pipeline
is sequential (`faddr == daddr+1 && nfaddr == daddr+2`); the two delay-slot
instructions behind a delayed branch go through the interpreter's `step`,
after which the pipeline is sequential again. All guest state lives in the
`Sharc` struct in memory and is consistent at every block exit (the pipeline
registers are compile-time constants inside a block, stored before helper
calls and at exits), so the coprocessor worker's savestate pause/sync sees
exactly the interpreter's state. Blocks end at control flow (jumps, calls,
RTS/RTI, IDLE), at non-internal fetch addresses, and at a 32-instruction
cap; interrupts are sampled between blocks.

The bus is type-erased through a vtable rather than by monomorphizing the
cache over `B: SharcBus`, because the worker thread's `BatchBus<'_>` borrows
and is not `'static`; both bus implementations (worker and single-threaded
lockstep) share the one cache. Uploaded/self-modifying microcode invalidates
via per-0x2000-word-page code epochs bumped by every internal PM write
(`Sharc::code_epochs`, runtime-only like the cache).

Lowered natively: `NOP` and compute-slot-less `Compute` (free), plus the
parts the interpreter pays per instruction that a block hoists to compile
time -- fetch, decode, the pipeline advance, and the FIFO-flag refresh
(emitted only before instructions that can observe FLAG0/1 through a
condition or an ASTAT read). Everything with architectural effect calls the
interpreter's own `dispatch` verbatim through a trampoline, and the hardware
`DO UNTIL` close is an inline guard that falls into `handle_loop` -- the
bottom-of-loop instruction still executes on the redirecting iteration, with
the redirected PC, exactly as the interpreter does. Lowering decisions
follow MAME's BSD-3 SHARC DRC front-end's basic-block contract
(`src/devices/cpu/sharc/sharcdrc.cpp`): end at control flow, fall back to C
for the rest.

Cycle accounting charges at block exits, so `icount` overshoots by up to 31
instructions per batch (the interpreter stops mid-block); the overshoot
burns `DO UNTIL FOREVER` park iterations and is architecturally invisible --
verified bit-identical: single-threaded lockstep (`--copro-mt off`) produces
identical screenshots and identical SHARC register/ASTAT/pipeline state
after `run 600` on vstriker, JIT vs interpreter (only the retired-instruction
counter moves). The sharc test suite runs through the JIT by default, and a
lockstep differential test (`jit_matches_interpreter`, single-instruction
blocks via `BLOCK_CAP`) diffs the full architected state against the
interpreter over a hardware-loop program.

Validation: `cargo test --workspace` green; `run 600` boots vf2, srallyc,
vstriker, schamp with the JIT on and off; `run 3600` vstriker soak;
savestate round trip on vf2 with the JIT on, and interpreter-saved states
load into a JIT machine (layout unchanged, FORMAT_VERSION 2). Timing
(`run 600`, median of 3, same session): vstriker 1.58s -> 1.40s (~12%);
vf2 parity (2A, SHARC idle).

Remaining work: native lowering of the hot compute/parallel-move classes
(the trampoline dominates block time), block chaining, and a runtime
dual-run harness like `I960_DUALRUN` (the in-crate differential covers the
loop machinery today). The MB86235 should follow the same path -- the
vtable/bus-erasure pattern and the epoch invalidation carry over unchanged --
but note its bus is already `'static`, so monomorphizing the cache like the
i960 does is also an option there.

### Status: step 4 landed for the MB86235 (feat/gottagofast)

The Fujitsu MB86235 "TGPx4" (Model 2C) has a Cranelift block dynarec
(`crates/mb86235/src/jit.rs`, feature `jit` on the mb86235 crate, default
on; runtime switch `Config::mb86235_jit` / `--mb86235-jit on|off`, mirrored
at savestate restore). It follows the SHARC JIT's architecture: blocks are
cached by entry address with per-256-word-page code epochs bumped by every
`upload_program_half`, the bus is type-erased through a vtable (the copro
worker's `BatchBus<'_>` borrows and is not `'static`), and all guest state
lives in the `Mb86235` struct in memory, consistent at every block exit, so
the worker's savestate pause/sync sees exactly the interpreter's state.
Blocks end at control flow (DJMP/DBcc/DCcc/DCALL/DRET/DBLP/DBBC/DBBS), at
REP arming (the repeat holds the PC at runtime), and at a 32-instruction
cap; the driver runs the interpreter's `step` for delay slots, REP
repetitions and input-FIFO stall retries -- the states where the fetch
address is not simply `pc`. A FIFO stall inside a block exits immediately
with `pc`/`stall_pc` consistent, and the driver retries through `step` like
the interpreter loop does. Lowering decisions follow MAME's BSD-3 MB86235
DRC front-end's basic-block contract (`src/devices/cpu/mb86235/
mb86235drc.cpp`); the interpreter remains the behavioural authority.

Lowered natively: the control-class NOP (free), the illegal class (a counted
fault), class-7 immediate-to-register transfers, and the register-only forms
of classes 0/1/4/5 (no FIFO, no external bus, no EA post-actions, sources
kept off the PR ring) whose ALU slot is one of the float/integer arithmetic,
compare, logical or shift operations (FEA/FES/FRCP/FRSQ/FLOG/CFIB excluded)
and whose multiplier slot is FMUL/IMUL. Everything else goes through a
trampoline that runs the interpreter's per-instruction bookkeeping and its
own `execute_op` verbatim, so nothing behaves differently. Flag updates
reproduce the interpreter's exact set/clear/sticky semantics; host f32
arithmetic is bit-identical to the interpreter's own.

Validation: `cargo test --workspace` green (71 tests). The mb86235 suite
gained two lockstep differentials (`jit_matches_interpreter`,
`jit_matches_interpreter_alu_sweep`, single-instruction blocks via
`BLOCK_CAP`, diffing the full architected state and asserting the JIT
compiled blocks) covering REP, call/return delay slots, DJMP, FIFO stalls,
every lowered ALU/multiplier form, the constant tables and the EB/ST
control-register transfers, plus an upload-invalidation test. `run 600`
boots hotd and waverunr with the JIT on and off; `run 3600` hotd soak;
savestate round trip on hotd with the JIT on, and interpreter-saved states
load into a JIT machine (layout unchanged, FORMAT_VERSION 2). Bit-exact:
single-threaded lockstep (`--copro-mt off`) produces identical screenshots
and identical TGPx4 register/PC/state dumps after `run 600` on hotd, JIT vs
interpreter (only the retired-instruction counter moves -- the JIT charges
`icount` at block exits, so batch overshoot burns extra park iterations).

Timing (`run 600`, median of 3, same session): hotd parity (1.63s both
engines -- i960-bound boot scene; 3.07s vs 3.10s in `--copro-mt off`
lockstep), waverunr 1.73s -> 1.65s (~5%).

Remaining work: native lowering of the memory-transfer forms (EA addressing
without bus side effects -- internal RAM A/B reads/writes are pure state
accesses), block chaining, and lowering stall-retry-friendly forms (DSP time
parked on an empty input FIFO retries through the interpreter today, which
caps the win on FIFO-bound scenes like hotd's boot).

## References

- Cranelift: https://github.com/bytecodealliance/wasmtime/tree/main/cranelift
- Gecko (Cranelift JITs in a Rust emulator): https://github.com/ioncodes/gecko
- MAME SHARC DRC: https://github.com/mamedev/mame/tree/master/src/devices/cpu/sharc
- MAME MB86235 DRC: https://github.com/mamedev/mame/tree/master/src/devices/cpu/mb86235
- MAME ARM64 DRC back-end: https://github.com/mamedev/mame/blob/master/src/devices/cpu/drcbearm64.cpp
- Supermodel threading and savestate pause: https://github.com/trzy/Supermodel/blob/master/Src/OSD/SDL/Main.cpp
- Dolphin DSP mailbox savestate: https://github.com/dolphin-emu/dolphin/blob/master/Source/Core/Core/DSP/DSPCore.cpp
- Dolphin on asynchronous audio timing: https://blog.delroth.net/2013/07/why-dolphin-is-getting-rid-of-asynchronous-audio-processing/
- Emu68 (M68K to AArch64 translator): https://github.com/michalsc/Emu68
