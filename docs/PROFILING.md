# Profiling results

Method: headless runs through the scriptable debugger (`--debug <set> -c
"run 600"`) under `perf record`, release build with line tables, desktop
x86-64 (AMD Ryzen 9 5900XT). Percentages are of total CPU time. ROM
decompression at startup accounts for 1-4% of every run and can be ignored.

Taken before the coprocessor was threaded, so every run here is the
single-threaded lockstep machine, which is deterministic and reproducible.

## What this profile can and cannot tell you

Read this first. The document originally carried conclusions its own method
could not support, and two of them were acted on.

**Trustworthy.** The percentages are `perf` samples of where CPU time
actually went. Sampling is independent of the frame-counting mistake
described in PERFORMANCE.md section 1, so it is not contaminated by it. The
wall-clock baselines are also sound *for these runs specifically*: in
lockstep a 600-frame run retires 99.4-99.9% of a full cycle budget (daytona
99.75%, vf2 99.88%, vstriker 99.37%, hotd 99.94%), so 600 frames really is
about 10 seconds of emulated machine time here, and the times below really
are comparable to each other.

**Not trustworthy, and the reason the branch went wrong.** That last property
holds *only* while the machine retires a full frame's worth of cycles every
frame. The original text stated the rule without its precondition -- "10
seconds of emulated time needs 10 seconds at full speed, so anything under
10s is above full speed" -- and these exact baselines then became the numbers
that threaded runs were compared against. Threaded runs retire as little as
84.6% of those cycles, so the comparison measured the machine skipping work
and reported it as speed. Always pair a timing here with a `cycles=` reading
from the debugger's `state` line.

**Blurred by inlining.** With thin LTO the coprocessor stepping inlines into
`run_slice`, so on vstriker the ~40% bucket below is scheduler *and* DSP work
with no way to separate them. That bucket also contains the per-quantum
overhead -- core swap in and out of the system struct, timer ticks, sound
tick, IRQ reconcile -- paid ~6,800 times a frame. Attributing all of it to
the DSPs is what motivated threading them. To split it, profile a build with
`lto = "off"` and `codegen-units = 1`, or add explicit `#[inline(never)]`
markers to the stepping functions, before drawing any conclusion from it.

**Desktop only.** Nothing here was measured on a phone, and the shortfall
being chased is a phone shortfall. Every percentage below may redistribute on
a different core count, cache size and memory system, and the renderer -- not
profiled at all here, because a headless run does not present -- is a
plausible mobile bottleneck that this method structurally cannot see.

## Daytona USA (Model 2 original, i960 + MB86233 TGP) — 1.70s

| Area | % |
|---|---|
| i960 interpreter (dispatch, execute, bus reads, addressing) | ~54 |
| Sound (board stepping, MultiPcm) | ~11 |
| `run_slice` (scheduling, incl. inlined DSP work) | ~9 |
| Geometry engine | ~3.5 |
| TGP (mb86233) | ~3 |

## Virtua Fighter 2 (Model 2A, i960 + MB86233 TGP) — 1.67s

| Area | % |
|---|---|
| i960 interpreter | ~55 |
| Sound (SCSP `generate` alone is 12.9) | ~21 |
| `run_slice` | ~6 |

## Virtua Striker (Model 2B, i960 + ADSP-21062 SHARC) — 2.78s

| Area | % |
|---|---|
| `run_slice` (scheduler + SHARC stepping, inlined together) | ~40 |
| i960 interpreter | ~29 |
| SHARC visible outside inlining | ~4.4 |
| Sound (SCSP) | ~7.5 |

## Virtua Fighter (Model 1, V60 + MB86233) — 1.59s

Spread evenly: mb86233 execute 7.4, V60 run 6.8, bus reads ~10, V60
addressing ~10, sound ~11, libc memory operations ~12 (framebuffer traffic),
Z80 sound CPU 1.8.

## Conclusions

1. **The i960 interpreter is the largest single cost on every Model 2 board**
   (29-55%). This is the profile's strongest result and it is independently
   corroborated: with work held equal, the i960 dynarec is worth 13% on
   daytona, 9% on vf2, 4% on vstriker and 13% on hotd -- the only measured
   win on the branch.

2. **The vstriker `run_slice` bucket is unattributed, not "coprocessor
   stepping".** The original conclusion read "coprocessor stepping is the
   second cost and dominates on 2B, where four DSPs run", and threading was
   built on it. Three things are wrong with that. The bucket cannot be split
   from scheduler overhead (see above). Model 2B in this emulator runs one
   SHARC, not four DSPs -- the "4x MB86235" in the original header was the
   Model 2C part, and vf2 was likewise labelled "2x SHARC" when 2A runs the
   MB86233 TGP; both are corrected above, from `roms_db.dat`. And when the
   DSP dynarecs were finally measured they moved nothing in the shipping
   configuration, which is what a DSP that is not on the critical path looks
   like.

3. Sound is 10-21%; SCSP sample generation is the worst single sound item
   (12.9% on 2A). Untouched so far, and on this evidence a better second
   target than the DSPs.

4. Model 1 has no single hotspot; it is the cheapest board overall.

## What to measure next

The profile that would actually direct this work does not exist yet:

- The same `perf` breakdown **on a target phone**, not a desktop.
- A **windowed** run rather than headless, so the rasterizer, tilemap and
  present path appear at all.
- A **non-inlined** build, so `run_slice` splits into scheduler versus
  coprocessor and the per-quantum overhead can be costed on its own.
- A `cycles=` reading beside every wall-clock number, so the runs being
  compared are known to have done the same work.
