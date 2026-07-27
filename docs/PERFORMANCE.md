# Performance plan

Current state: every processor core (i960, V60, SHARC ADSP-21062, MB86233,
MB86235, 68000) is an interpreter with match-based dispatch, and all chips run
sequentially on one thread. On desktop this is enough; on Android phones games
run below full speed.

This document is the second version of the plan. The first one produced
`feat/gottagofast`, whose headline result -- "daytona 1.70s -> 1.15s, vf2
1.67s -> 1.13s, vstriker 2.78s -> 1.31s" -- was an artifact of measuring the
wrong quantity. The work was not slow to write and not obviously wrong to
read; it was wrong to *believe*, and the belief survived because the number
that was supposed to check it could not. So the measurement rules come first
here, before any plan, and the post-mortem is kept in full rather than
summarised away.

Everything below was measured on an AMD Ryzen 9 5900XT (16 cores / 32
threads), x86-64 Linux. **No measurement in this document was taken on a
low-powered device.** That is the single largest gap in it.

## 1. What "speed" means, and why frame counts do not mean it

A frame counter is not a measure of work. `Model2System::run_slice` skips the
i960 for a whole quantum when the coprocessor input FIFO is backed up
(`system.rs`, the `copro_fifo_in_len() <= COPRO_FIFO_DEPTH` guard), and an
empty output FIFO read ends its quantum early through `main_stall`. The
timers and the sound board still advance by that quantum either way. So a
frame can complete having retired a fraction of a frame's cycles: the board's
clocks move forward while the CPU does nothing.

The consequence is that **timing a fixed number of frames rewards doing less
work**. A change that makes the machine skip more CPU time makes the benchmark
finish sooner and reports as a speedup. That is precisely what happened.

The honest metric is retired cycles:

```
real speed = (i960 cycles retired in the window / elapsed seconds) / CYCLES_PER_FRAME
```

A `step()` that skipped the CPU contributes nothing to it, so it cannot be
inflated. It is implemented in `crates/tgpulse/src/app.rs` and logged under
the `stats` target, and the debugger's `state` line carries the same counter
as `cycles=` for headless work:

```sh
RUST_LOG=warn,stats=info ./tgpulse --roms ../roms daytona
# REAL 57.5 fps (100% speed) | counted: video 60.0 emulated 58.0 | ...
```

The `counted:` half is retained deliberately, as a permanent illustration:
across the interpreter, the threaded and the threaded+JIT configurations it
prints `video 60.0 emulated 58.0` for all three, while the real figure moves
between 57.5 and 46.3. Any metric that cannot separate those configurations
is not a performance metric.

Two further rules, both learned the hard way:

- **Never report a bare average.** An earlier round of instrumentation logged
  0.5-second means of presented and emulated frames and read "video 60,
  emulated 58" while the game visibly juddered, because `advance()` bursts up
  to `MAX_CATCH_UP` frames per displayed frame: stall, fast-forward, repeat.
  Report worst case and distribution -- max step time, budget overruns,
  catch-up bursts -- alongside any mean.
- **Speed claims need a work witness.** State the retired-cycle count next to
  every timing. Two configurations may only be compared by wall time if they
  retired the same cycles; otherwise compare cycles per second, or the
  comparison is meaningless.

## 2. How to run a performance test

1. **Establish determinism first.** Run the same workload three times and hash
   the output frame (`-c "run 600; screenshot out.ppm"`). If the hashes differ,
   stop: nothing timed against this configuration means anything yet, and the
   nondeterminism is itself the bug to fix. The single-threaded path satisfies
   this today -- five runs of daytona are bit-identical.
2. **Pin the work.** Record `cycles=` from `state` for both sides. If they
   differ by more than a fraction of a percent, the wall-clock comparison is
   invalid; normalise to cycles per second and say so.
3. **Check the picture, not just the clock.** Compare screenshots against the
   reference configuration. A change that alters output is an accuracy
   regression until proven otherwise, whatever it does to the clock.
4. **Measure the thing the player feels.** Wall time for N frames is a
   throughput measure and hides stalls. For interactive behaviour use the real
   fps line above, over at least 20 seconds, and look at the minimum.
5. **Measure on the target.** Desktop headroom hides everything. A phone is
   the device this work exists for; a 32-thread desktop is not a proxy for it.

A useful sanity check that needs no instrumentation at all: how far does the
game actually get? Capture frames at fixed intervals and diff consecutive
images. If a configuration produces less motion per emulated frame, it is
running the game slower no matter what the frame counter says. This is what
finally exposed the branch -- VF2 threaded is still on the boot screen at
frame 650 where the interpreter is running the attract demo.

## 3. Accuracy gates

These are not optional and none of them may be waived for a speedup.

- **Bit-exactness is verified in the configuration that ships.** The branch
  verified its JITs under `--copro-mt off` while shipping `--copro-mt on`.
  That validated a path no user runs. Whatever is default is what must be
  proven.
- **Determinism is a correctness property.** Same input, same output, every
  run. A configuration that cannot reproduce its own frame cannot be
  regression-tested, savestated reliably, or debugged from a report.
- **The interpreter stays the behavioural authority.** Any dynarec falls back
  to it per instruction for anything not lowered, and a lockstep differential
  test diffs full architected state.
- **Savestates round-trip in both directions**, including states written by
  the other engine.
- **A golden-frame test per board.** One screenshot hash per game per fixed
  frame count, checked in CI. The branch's whole failure would have been
  caught by this on day one: daytona under threading produces a different
  frame on 25-40% of runs.

## 4. Post-mortem: what went wrong on feat/gottagofast

The branch is six commits: profiling, coprocessor threading, and Cranelift
dynarecs for the i960, SHARC and MB86235.

**The threading commit is the defect.** Measured with the real-speed metric,
daytona in the GUI:

| configuration | real speed | old counters | smallest display list |
| --- | --- | --- | --- |
| interpreter, lockstep | 57.5 fps (100%) | video 60.0 / emulated 58.0 | 308 words |
| threaded, no JIT | 46.5 fps (81%) | video 60.0 / emulated 58.0 | 9 words |
| threaded + all JITs | 46.3 fps (80%) | video 60.0 / emulated 58.0 | 9 words |

Two failures, one cause:

- **It is slower.** The i960 retires 84.6% (daytona) to 96.6% (hotd) of the
  cycles it retires in lockstep, because it is skipped whenever the worker
  falls behind on the FIFO. VF2 is far worse than the average suggests: it
  produces ~17x less motion per frame and is roughly two attract screens
  behind at frame 900.
- **It corrupts the picture.** The display list collapses from ~17,300 words
  to 9 on some frames -- a scene that never got built, seen as flashing. In
  headless runs 25-40% of daytona runs end on a frame with the entire 3D scene
  missing. It reproduces with every JIT disabled, and `--copro-mt off` is
  bit-identical across five runs, so it is the threading, not the dynarecs.

The mechanism is a broken producer/consumer contract. The FIFOs pace the i960
against the DSP, and that part is modelled carefully. But nothing paces the
DSP against the *rasterizer*: `trigger_vblank` snapshots `buffer_ram` and
parses it whenever the frame ends, while the worker free-runs. In lockstep the
DSP had deterministically finished the list by then. Taking the coprocessor
lock around the snapshot does **not** fix it -- tested, still 3 of 10 runs bad
-- because the list is not torn, it is genuinely unfinished. The code comment
above that parse already warned about "the intermittent exploded/inverted
scenes" from reading the buffer while it is rebuilt; threading reintroduced
exactly that hazard from the other side.

**The dynarecs are not where the reported gains came from.** Held to equal
work in lockstep mode (cycles within 0.2%), the i960 JIT is worth 13%
(daytona 1.79s -> 1.56s), 9% (vf2), 4% (vstriker) and 13% (hotd). Real, useful,
and an order of magnitude smaller than the branch's headline. The other two
measured nothing at all:

| | threaded (shipping default) | lockstep |
| --- | --- | --- |
| `--sharc-jit` on / off | 1.35 / 1.36 s | 4.10 / **3.02** s |
| `--mb86235-jit` on / off | 1.69 / 1.68 s | 3.20 / 3.23 s |

In the configuration that ships they are worth nothing, because the worker is
off the critical path -- the i960 is. In lockstep the SHARC JIT is a 36%
*regression*. Two of the six commits are pure risk: ~1,800 lines of new
unvalidated code generation on the worker thread for no measured payoff.

**The off-switches do not restore the old path.** The branch states that each
`off` flag "restores the exact pre-branch code path". Behaviourally true --
all four games produce screenshots bit-identical to the `Android` branch. But
with everything off it is 5-11% *slower* than `Android` (daytona 1.69s vs
1.57s, vf2 1.72s vs 1.63s, vstriker 2.88s vs 2.70s, hotd 3.62s vs 3.27s). The
refactor taxed the fallback.

**Process failures worth naming, because they are the reusable part:**

- The benchmark was chosen before it was known what it measured. `run 600` is
  a fine *throughput* harness for a fixed workload and a broken *speed*
  harness for a variable one, and nothing in it announced the difference.
- Validation was run in the non-default configuration.
- Batch size (`BATCH_CYCLES = 8`) was swept against the interpreter and never
  revisited once the JITs changed the cost of a batch.
- "Expected gain: 1.5-2.5x" was asserted from the pattern's reputation
  elsewhere, not derived from a profile of this codebase. No Amdahl bound was
  computed, so there was nothing to falsify the result against.
- The user's report ("it's slow") was weighed against the instrumentation and
  lost, twice. The instrumentation was wrong both times. When a player's
  perception and a counter disagree, the counter is the hypothesis.

## 5. What to do with the branch

- **Keep** the i960 dynarec. It is the only measured win, it is deterministic,
  and its own status notes are honest about its remaining work.
- **Keep** the real-speed instrumentation (commit `0502403`, currently unpushed
  on that branch). Nothing else on the branch can be evaluated without it.
- **Revert** the coprocessor threading. It causes both reported symptoms and
  its design cannot be repaired by a lock; see the next section for what a
  correct version would require.
- **Revert or shelve** the SHARC and MB86235 dynarecs. They may be worth
  revisiting if the DSP ever becomes the critical path on a real device, but
  they should not ship on measurements that do not show a gain.
- **Fix** the 5-11% tax the refactor put on the interpreter path, or unwind the
  abstraction that caused it.

## 6. The corrected plan for low-powered devices

The ordering principle: cheapest and most certain first, and nothing proceeds
without a profile that bounds what it can win.

1. **Profile on the target device, and compute the bound.** Split a frame into
   i960, geometry DSP, rasterizer, tilemap, audio and present. Until that
   split exists for a phone, every estimate in this document is a guess --
   including the ones below. Amdahl's law then caps each candidate: if the
   rasterizer is 60% of a phone frame, no CPU dynarec can do better than 1.6x,
   and the effort belongs in the renderer. This step is cheap and it decides
   everything after it.

2. **Reduce per-quantum overhead.** The scheduler runs a 64-cycle quantum with
   a fixed cost each time -- core swap in and out of the system struct, timer
   ticks, sound tick, IRQ reconcile. That cost is paid ~6,800 times a frame
   regardless of how fast the cores are, and it is deterministic work with no
   accuracy risk in reducing it. Measure it before assuming it is small.

3. **Finish the i960 dynarec.** Its own notes name the remaining work: every
   block exit re-enters the dispatcher, and call/ret is not lowered, so
   call-heavy code still pays the fallback. Block chaining and call/ret are
   the obvious next gains on the one core that is demonstrably the critical
   path, with an existing differential harness to keep it honest.

4. **Look hard at the renderer before threading anything.** On a phone the
   rasterizer and the present path are the likeliest bottleneck, and they are
   the part with the most headroom that costs no accuracy: the geometry is
   already parsed into a display list, and `trigger_vblank` already hands the
   rasterizer an owned snapshot. Rendering is where parallelism is safe here,
   precisely because that snapshot boundary already exists.

5. **Only then reconsider threading, and only with a frame contract.** The
   lesson from the failure is not "threading is impossible", it is that the
   geometry DSP has *two* consumers -- the i960 through the FIFOs, and the
   rasterizer through `buffer_ram` at vblank -- and the branch modelled only
   the first. A correct design must add a barrier: at `trigger_vblank` the
   main thread waits until the worker has drained the frame's queued input and
   completed its pending writes. That preserves within-frame overlap, which is
   the actual win, while restoring the guarantee that a display list is
   finished before it is parsed.

   Be honest about the cost: free-running threads make the FIFO interleaving
   irreproducible, so a threaded coprocessor cannot be bit-deterministic even
   with the barrier. That is why it must stay opt-in, with lockstep as the
   default and the reference, and why it is last on this list rather than
   second.

6. **Pin to big cores on Android** (`sched_setaffinity`) once there is a
   threaded configuration worth pinning. Not before.

The 68000 stays interpreted: it runs sound, it is cheap, and it is
audio-rate-coupled.

## 7. Implementation record

What was actually built on this branch, kept because it is an accurate
description of the code and the only such description that exists. Section 5
says what each piece is worth; this says what each piece *is*.

Read the timing figures inside these sections with the warning in section 1
in mind: they were taken with the `run 600` harness, so they measure frames
rather than work and none of them can be trusted as a speed claim. The
descriptions of what is lowered, what falls back to the interpreter, how
invalidation works and what was validated are unaffected by that and remain
correct.

### The i960 dynarec

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

### The SHARC dynarec

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

### The MB86235 dynarec

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

### Why the MB86233 stays interpreted

The MB86233 TGP (Model 1, and the original Model 2 board) is the one
coprocessor without a dynarec, deliberately:

- Profiling puts it at ~3% of frame time on daytona, and Model 1 is the
  cheapest board overall (docs/PROFILING.md). The wins left here are
  small.
- Unlike the SHARC and MB86235, no reference DRC exists anywhere (MAME has
  only an interpreter), so the lowering would be unreferenced work against
  the least rewarding target.

If a future profile says otherwise, the SHARC/MB86235 JIT pattern carries
over; the gating and bus-sharing machinery is already generic.

## Prior art and back-end choice

This section is unchanged from the first plan; the research held up, only the
conclusions drawn from it did not.

- **No dynarec exists for the i960, V60 or MB86233**, in any language. The only
  i960 recompiler ever written is the closed-source x86 one in ElSemi's Model 2
  Emulator (last release 2014). These front-ends must be written regardless of
  approach.
- **MAME has DRC front-ends for the SHARC and MB86235** (`src/devices/cpu/
  sharc/sharcdrc.cpp`, `src/devices/cpu/mb86235/mb86235drc.cpp`) and an ARM64
  DRC back-end (`drcbearm64.cpp`, on asmjit). All BSD-3-Clause. Tied to MAME's
  device framework and not importable, but a usable lowering blueprint.
- **68000 has several fast cores** (Amiberry and Emu68 have AArch64 JITs).
  Not needed, per above.

`cranelift-jit` remains the back-end: pure Rust, AArch64 tier-1, Apache-2.0
with LLVM exception, compile latency designed for JIT workloads, and proven in
Rust emulators (Gecko runs Cranelift JITs for PowerPC and the GameCube DSP;
RustEE offers a Cranelift backend for the PS2). On Android JIT is permitted --
Dolphin, PPSSPP and Flycast ship dynarecs -- and W^X is handled by Cranelift's
memory API. Alternatives rejected: LLVM via inkwell (compile latency, painful
Android linking), libgccjit (runtime shared-library dependency, GPL), QEMU
libtcg (no maintained upstream library), dynarmic (guest ARM only), GNU
lightning (GPLv3), dynasm-rs (MPL-2.0, little maintenance), asmjit (capable,
but C++ FFI and a full code generator to write; kept as fallback).

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
