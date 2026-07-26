# Profiling results

Method: headless runs through the scriptable debugger (`--debug <set> -c
"run 600"`, 600 frames = 10 emulated seconds) under `perf record`, release
build with line tables, desktop x86-64. Percentages are of total CPU time.
ROM decompression at startup accounts for 1-4% of every run and can be
ignored. Real time for 600 frames is included as the speed baseline; 10
seconds of emulated time needs 10 seconds at full speed, so anything under
10s is above full speed on this machine. Phones are several times slower,
which is where the shortfall comes from.

## Daytona USA (Model 2, i960 + TGP) — 1.70s

| Area | % |
|---|---|
| i960 interpreter (dispatch, execute, bus reads, addressing) | ~54 |
| Sound (board stepping, MultiPcm) | ~11 |
| `run_slice` (scheduling, incl. inlined DSP work) | ~9 |
| Geometry engine | ~3.5 |
| TGP (mb86233) | ~3 |

## Virtua Fighter 2 (Model 2A, i960 + 2x SHARC) — 1.67s

| Area | % |
|---|---|
| i960 interpreter | ~55 |
| Sound (SCSP `generate` alone is 12.9) | ~21 |
| `run_slice` | ~6 |

## Virtua Striker (Model 2B, i960 + 4x MB86235) — 2.78s

| Area | % |
|---|---|
| `run_slice` (with thin LTO the coprocessor stepping inlines into it) | ~40 |
| i960 interpreter | ~29 |
| SHARC visible outside inlining | ~4.4 |
| Sound (SCSP) | ~7.5 |

## Virtua Fighter (Model 1, V60 + 5x MB86233) — 1.59s

Spread evenly: mb86233 execute 7.4, V60 run 6.8, bus reads ~10, V60
addressing ~10, sound ~11, libc memory operations ~12 (framebuffer
traffic), Z80 sound CPU 1.8.

## Conclusions

1. The i960 interpreter is the largest single cost on every Model 2 board
   (30-55%). This confirms the plan: the i960 dynarec is the highest-value
   item.
2. Coprocessor stepping is the second cost and dominates on 2B, where four
   DSPs run. Threading the coprocessors attacks exactly this.
3. Sound is 10-21%; SCSP sample generation is the worst single sound item
   (12.9% on 2A). Out of scope for now, noted for later.
4. Model 1 has no single hotspot; it is the cheapest board overall.
