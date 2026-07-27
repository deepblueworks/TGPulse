# Performance plan

Goal: full speed on low-powered devices without giving up accuracy. On desktop
the interpreter is already fast enough; on Android phones games run below full
speed, and that is the entire reason this work exists.

This is the second version of the plan. The first produced `feat/gottagofast`,
whose headline result -- "daytona 1.70s -> 1.15s, vf2 1.67s -> 1.13s, vstriker
2.78s -> 1.31s" -- was an artifact of measuring the wrong quantity. The
coprocessor threading and two of the three dynarecs have since been reverted.
The measurement rules therefore come first here, ahead of any plan, because a
plan is worth nothing if the numbers meant to check it cannot.

All measurements below are desktop x86-64 (AMD Ryzen 9 5900XT). **Nothing here
was measured on a low-powered device**, which is the largest gap in this
document and step 3 of the roadmap.

## 1. What "speed" means, and why frame counts do not mean it

A frame counter is not a measure of work. `Model2System::run_slice` skips the
i960 for a whole quantum when the coprocessor input FIFO is backed up, and an
empty output FIFO read ends its quantum early through `main_stall`. The timers
and the sound board advance by that quantum either way, so a frame can complete
having retired a fraction of a frame's cycles: the board's clocks move forward
while the CPU does nothing.

So **timing a fixed number of frames rewards doing less work**. A change that
makes the machine skip more CPU time finishes the benchmark sooner and reports
as a speedup. That is exactly what happened.

The honest metric is retired cycles:

```
real speed = (i960 cycles retired in the window / elapsed seconds) / CYCLES_PER_FRAME
```

A `step()` that skipped the CPU contributes nothing to it. It is implemented in
`crates/tgpulse/src/app.rs` and logged under the `stats` target; the debugger's
`state` line carries the same counter as `cycles=` for headless work.

```sh
RUST_LOG=warn,stats=info ./tgpulse --roms ../roms daytona
# REAL 57.5 fps (100% speed) | counted: video 60.0 emulated 58.0 | ...
```

The `counted:` half is kept deliberately, as a standing illustration: across
the interpreter, the threaded and the threaded+JIT configurations it printed
`video 60.0 emulated 58.0` for all three while the real figure moved between
57.5 and 46.3. A metric that cannot separate those is not a performance metric.

**The real-fps figure is capped and saturates at 100%.** In the windowed app
emulation is paced to `FRAME_NANOS` and the present is vsynced, so it can never
read above full speed. It is a pass/fail -- "does this device keep up" --
meaningful only when it drops below 100%. It cannot show headroom, which is why
gains must be measured with the uncapped headless harness, and why a desktop
cannot answer any question about a phone: every non-broken configuration pins
at the cap.

Two further rules:

- **Never report a bare average.** An earlier round of instrumentation logged
  0.5-second means and read "video 60, emulated 58" while the game visibly
  juddered, because `advance()` bursts up to `MAX_CATCH_UP` frames per
  displayed frame: stall, fast-forward, repeat. Report worst case and
  distribution alongside any mean.
- **Every speed claim needs a work witness.** Put the retired-cycle count next
  to the timing. Two configurations may be compared by wall time only if they
  retired the same cycles; otherwise compare cycles per second, or say nothing.

## 2. How to run a performance test

1. **Establish determinism first.** Run the workload three times and hash the
   output frame (`-c "run 600; screenshot out.ppm"`). If the hashes differ,
   stop -- the nondeterminism is the bug, and nothing timed against that
   configuration means anything yet.
2. **Check the switch actually switches.** Confirm the two configurations
   produce different output or different cycle counts before trusting any
   comparison between them. A reverted config-plumbing path once made
   `--i960-jit off` silently ignored in headless runs, so an A/B compared a
   configuration against itself and looked like a regression somewhere else
   entirely.
3. **Pin the work.** Record `cycles=` from `state` for both sides. If they
   differ by more than a fraction of a percent, normalise to cycles per second
   and say so.
4. **Interleave and repeat.** Best-of-3 or better, alternating configurations
   within one session. Absolute timings on this machine drift 15% or more
   between sessions; only interleaved comparisons survive that.
5. **Compare the picture, not just the clock.** Hash the frame against the
   reference configuration. A change that alters output is an accuracy
   regression until proven otherwise, whatever it does to the clock.
6. **Measure on the target.** See the cap above: desktop headroom hides
   everything that matters here.

A check that needs no instrumentation at all: capture frames at fixed intervals
and diff consecutive images. A configuration producing less motion per emulated
frame is running the game slower whatever the frame counter says. That is what
finally exposed the branch.

## 3. Accuracy gates

Not optional, and not waivable for a speedup.

- **Bit-exactness is proven in the configuration that ships.** The branch
  verified its JITs under `--copro-mt off` while shipping `--copro-mt on`,
  validating a path no user ran.
- **Determinism is a correctness property.** Same input, same output, every
  run. A configuration that cannot reproduce its own frame cannot be
  regression-tested, savestated reliably, or debugged from a bug report.
- **The interpreter is the behavioural authority.** A dynarec falls back to it
  per instruction for anything not lowered, and a lockstep differential diffs
  full architected state.
- **Savestates round-trip both ways**, including states written by the other
  engine.
- **A golden-frame test per board, in CI.** One screenshot hash per game at a
  fixed frame count. This single test would have caught the whole failure on
  day one: threaded daytona produces a different frame on 25-40% of runs.

## 4. Post-mortem: what the first plan got wrong

**Coprocessor threading (reverted).** Slower and visually broken. The i960
retired 84.6% (daytona) to 96.6% (hotd) of the cycles it retires in lockstep;
daytona ran at 80% of full speed against 100%; VF2 produced ~17x less motion
per frame and was two attract screens behind at the same frame number. The
display list collapsed from ~17,300 words to 9 on 25-40% of runs, seen as
flashing, and three identical runs produced three different frames. The cause:
the FIFOs pace the i960 against the DSP, but nothing paced the DSP against the
rasterizer, which parses `buffer_ram` at vblank whenever the frame ends.
Holding the coprocessor lock across that snapshot does not help -- the list is
not torn, it is unfinished.

**SHARC and MB86235 dynarecs (reverted).** Worth nothing in the configuration
they shipped in (1.35s vs 1.36s; 1.69s vs 1.68s), because those DSPs are not on
the critical path -- the i960 is. The SHARC one was a 36% regression in
lockstep. ~1,800 lines of code generation carrying real risk for no measured
gain.

**Process failures, which are the reusable part:**

- The benchmark was chosen before anyone knew what it measured. `run 600` is a
  fine throughput harness for fixed work and a broken speed harness for
  variable work, and nothing about it announced the difference.
- Validation ran in the non-default configuration.
- `BATCH_CYCLES = 8` was swept against the interpreter and never revisited once
  the JITs changed what a batch costs.
- "Expected gain: 1.5-2.5x" was asserted from the pattern's reputation
  elsewhere rather than derived from a profile, so no result could contradict
  it.
- A player's report of slowness was weighed against the instrumentation and
  lost, twice, while the instrumentation was the broken thing. **When
  perception and a counter disagree, the counter is the hypothesis.**

## 5. Where the branch stands now

Only the i960 dynarec survives. Measured interleaved, best of 3, against the
`Android` branch:

| game | Android | interpreter | + i960 JIT | net |
| --- | --- | --- | --- | --- |
| daytona | 1607 ms | 1665 ms | 1528 ms | **+4.9%** |
| hotd | 3328 ms | 3500 ms | 3067 ms | **+7.8%** |
| vf2 | 1623 ms | 1713 ms | 1658 ms | **-2.2%** |
| vstriker | 2704 ms | 2854 ms | 2939 ms | **-8.7%** |

The interpreter path is bit-identical to `Android` on all four games, so the
revert is clean. Two problems remain, and they are steps 1 and 2 below:

- **The dynarec taxes the interpreter path by 3.6-5.5%**, visible in the middle
  column, which is slower than `Android` everywhere despite being behaviourally
  identical to it. The cost is `bump_code_epoch` on every write to `ram_low`
  and `work_ram` -- invalidation bookkeeping paid whether or not the JIT is
  enabled. It gives back roughly half of what the JIT earns.
- **The dynarec is not bit-exact.** With the JIT on, daytona and vf2 match the
  interpreter exactly; **vstriker and hotd do not** -- deterministically, but
  differently. Those are the SHARC and TGPx4 boards, where the i960 stalls
  against the coprocessor FIFOs, so the JIT's block-granularity quantum
  accounting lands stalls in different places. Retired cycles differ by ~0.15%
  on vstriker, the same effect seen from the other side.

Net: on this desktop the branch is worth +5 to +8% on two boards and -2 to -9%
on the other two, at the cost of an accuracy divergence on half the tested
games. Not yet a shippable trade.

## 6. Roadmap

Ordered by certainty and cost, not by appeal. Nothing after step 3 begins
without step 3's numbers.

1. **Make the i960 dynarec bit-exact, or default it off until it is.** An
   accuracy gate is already being violated on vstriker and hotd. Either charge
   cycles and sample interrupts so stalls land where the interpreter puts them,
   or ship `i960_jit: false` and treat the JIT as opt-in until they match. The
   test is a whole-machine golden-frame differential across all four boards:
   the existing `I960_DUALRUN` harness covers the instruction level and did not
   catch this, because it does not run a board.

2. **Remove the invalidation tax.** `bump_code_epoch` runs on every RAM write
   even with the JIT disabled, costing 3.6-5.5%. Gate it behind
   `config.i960_jit`, or track only pages a block has actually been compiled
   from, or fold the check into the existing write path rather than a second
   call. Cheapest measurable win available, with no new correctness surface --
   pure overhead removal on a path already proven bit-identical.

3. **Profile on a target phone, and compute the bound.** Split a frame into
   i960, geometry DSP, rasterizer, tilemap, audio and present, on the device
   this work exists for. Until that split exists every estimate here is a
   guess. Amdahl then caps each later candidate: if the rasterizer is 60% of a
   phone frame, no CPU dynarec beats 1.6x and the effort belongs in the
   renderer. Profile a non-inlined build (`lto = "off"`, or `#[inline(never)]`
   on the stepping functions), because thin LTO merges the coprocessor into
   `run_slice` and makes that bucket unattributable -- the mistake that
   justified threading. See docs/PROFILING.md.

4. **Reduce per-quantum overhead.** The scheduler runs a 64-cycle quantum with
   a fixed cost each time -- core swap in and out of the system struct, timer
   ticks, sound tick, IRQ reconcile -- paid ~6,800 times a frame regardless of
   how fast the cores are. Deterministic work, no accuracy risk in reducing it.
   Step 3 says whether it is worth doing.

5. **Finish the i960 dynarec.** Its own notes name the remaining work: every
   block exit re-enters the dispatcher, and call/ret is not lowered, so
   call-heavy code still pays the fallback. Block chaining and call/ret are the
   next gains on the one core demonstrably on the critical path -- but only
   after step 1, since a faster incorrect JIT is worth less than a slower
   correct one.

6. **Look at the renderer before threading anything.** On a phone the
   rasterizer and present path are the likeliest bottleneck and carry the most
   headroom at no accuracy cost. `trigger_vblank` already hands the rasterizer
   an owned snapshot, so that boundary is where parallelism is safe here.

7. **Only then reconsider threading, with a frame contract.** The lesson is not
   that threading is impossible; it is that the geometry DSP has *two*
   consumers -- the i960 through the FIFOs, and the rasterizer through
   `buffer_ram` at vblank -- and only the first was modelled. A correct design
   adds a barrier: at `trigger_vblank` the main thread waits until the worker
   has drained the frame's queued input and completed its pending writes. That
   keeps the within-frame overlap, which is the actual win, while guaranteeing
   a display list is finished before it is parsed.

   The honest cost: free-running threads make the FIFO interleaving
   irreproducible, so a threaded coprocessor cannot be bit-deterministic even
   with the barrier. It stays opt-in with lockstep as the default and the
   reference, which is why it is last rather than second.

8. **Pin to big cores on Android** (`sched_setaffinity`) once there is a
   threaded configuration worth pinning. Not before.

The 68000 stays interpreted: it runs sound, it is cheap, and it is
audio-rate-coupled.

## Worth keeping from the first attempt

- The runtime engine switches (`--i960-jit`, and whatever follows it). Every
  diagnosis in this episode depended on A/B-ing a single change at runtime.
  Build them for anything new, and check they actually take effect (section 2,
  rule 2).
- The retired-cycle instrumentation, the only reason any of this was
  measurable.
- The lockstep differential pattern from the reverted DSP dynarecs: build the
  differential with the dynarec, not after it. Its weakness is now known too --
  an instruction-level differential does not catch a whole-machine timing
  divergence, which is what step 1 exists to fix.
- The reverted work is recoverable from history if a phone profile ever puts a
  DSP on the critical path: the vtable bus erasure and per-page code epochs
  carry over unchanged.

## Prior art and back-end choice

The research below held up; only the conclusions drawn from it did not.

- **No dynarec exists for the i960, V60 or MB86233**, in any language. The only
  i960 recompiler ever written is the closed-source x86 one in ElSemi's Model 2
  Emulator (last release 2014).
- **MAME has DRC front-ends for the SHARC and MB86235** (`src/devices/cpu/
  sharc/sharcdrc.cpp`, `src/devices/cpu/mb86235/mb86235drc.cpp`) and an ARM64
  DRC back-end (`drcbearm64.cpp`, on asmjit). All BSD-3-Clause, tied to MAME's
  device framework, usable as a lowering blueprint if a phone profile ever puts
  a DSP on the critical path.
- **68000 has several fast cores** (Amiberry, Emu68). Not needed.

`cranelift-jit` remains the back-end: pure Rust, AArch64 tier-1, Apache-2.0
with LLVM exception, compile latency designed for JIT workloads, proven in Rust
emulators (Gecko for PowerPC and the GameCube DSP; RustEE for the PS2). On
Android JIT is permitted -- Dolphin, PPSSPP and Flycast ship dynarecs -- and
W^X is handled by Cranelift's memory API. Rejected: LLVM via inkwell (compile
latency, painful Android linking), libgccjit (runtime dependency, GPL), QEMU
libtcg (no maintained upstream library), dynarmic (guest ARM only), GNU
lightning (GPLv3), dynasm-rs (MPL-2.0, little maintenance), asmjit (C++ FFI and
a full code generator to write; kept as fallback).

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
