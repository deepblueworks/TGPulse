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
