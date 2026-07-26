# <img src="assets/logo.png" alt="TGPulse Emulator" width="420">

An emulator for Sega's Model 1 and Model 2 arcade boards, written in Rust.

Every processor on those boards is emulated instruction by instruction: the
NEC V60 and MB86233 "TGP" of the Model 1, and the Intel i960, the TGP, the
ADSP-21062 SHARC and the MB86235 "TGPx4" across the Model 2 revisions. The
geometry engine is not a processor — no dump of it exists — so its display-list
command stream is decoded behaviourally, as MAME's is. Pixel coverage is a
hardware model in a compute shader rather than a modern rasterizer, so the
image is the board's own, dither patterns and all.


## Running

Build it, put some ROMs where it can find them, and start it:

```sh
cargo build --release
mkdir -p roms
cp ~/wherever/vf2.zip roms/
./target/release/tgpulse
```

**Where the ROMs go.** One zip per romset, in a directory called `roms/` beside
the working directory -- `--roms <dir>` points somewhere else. The archives are
the individual chip dumps, zipped, exactly as they are usually distributed;
leave them zipped and do not rename anything inside. The file name itself
carries no weight, because each archive is identified by matching its contents
against the ROM database, so `vf2.zip`, `Virtua Fighter 2.zip` and
`somefile.zip` are all the same set as far as this is concerned.

ROMs are not distributed with this program and none are included in this
repository. You need to own the boards or otherwise have the right to the
data.

**Starting a game.** With no arguments the emulator opens on its attract screen
with the library window in the middle: it lists what it found in `roms/`, what
each one is, and whether anything is missing from it. Double-click a row to
run it. If you add archives while it is open, press Refresh. From a terminal
you can also skip the library entirely:

```sh
./target/release/tgpulse vf2        # by set name
./target/release/tgpulse --list     # what is in roms/, without opening a window
./target/release/tgpulse --help     # every option
```

**Playing.** Insert a coin with `5` (Select on a pad), then start with `Enter`
(Start on a pad). Movement is the arrow keys, WASD, the d-pad or the left
stick; the action buttons are `J`/`K`/`L` and West/South/East. Driving games
steer with left and right and use the triggers for throttle and brake, and the
gun games aim with the mouse and fire with `Space`. Every one of these is
rebindable under Settings -> Input, keyboard and gamepad alike, and what you
choose is written to `config/input.conf`.

Some games want their own settings before they will behave -- the test menu is
`F2`, and the service switch is `F8`.

**Hotkeys.**

| Key | |
| --- | --- |
| `F1` | show or hide the interface over a running game |
| `F3` | reset the machine |
| `F5` / `F7` | save and load the current state slot |
| `F4` / `F6` | previous and next slot |
| `F9` | pause |
| `F11` | fullscreen |
| `Tab` | fast forward while held |

**What it writes.** Battery-backed RAM -- high scores, rankings, and whatever
the test menu was set to -- goes to `nvram/<set>.nv` when the game is closed,
and is read back on the next run. Save states go to `states/`, input bindings
and options to `config/`. Nothing is written anywhere else, so the whole thing
moves by copying the directory.

## Rendering

The board drew 496x384 with no antialiasing at all. TGPulse reproduces its
rasterizer exactly -- the dither patterns, the stipple transparency, the
painter-sort artefacts -- and then draws it far larger than the cabinet could.

**High definition.** The 3D layer is rasterized on a denser grid and averaged
back down, at up to four samples per output pixel (`--ssaa 1..4`, default 2),
and the result is presented at twice the board's resolution. Polygon edges come
out clean while the tile layers -- the HUD, the text, the sky -- are scaled by
whole pixels and stay exactly as sharp as the hardware drew them. `--ssaa 1` is
the board's own output, pixel for pixel, if that is what you want.

```sh
./target/release/tgpulse daytona --ssaa 4
```

**Widescreen.** `--widescreen on` renders 16:9 by widening the field of view
rather than stretching the 4:3 picture: the frustum and the viewport open up
around the centre, so more of the scene is visible at the sides instead of
everything being fatter. The 2D tile layers stretch to fill by default
(`--widescreen-stretch-2d off` leaves them centred).

This is a hack, not hardware behaviour. Games were composed for 4:3 and some of
them show it -- HUD elements pinned to the screen edge, scenery that stops
where the original camera stopped.

**Smooth shadows.** The board has no alpha channel: shadows are a checkerboard
stipple and translucent textures are a per-texel coverage that thresholds.
`--smooth-shadows off` reproduces that exactly; on, the default, blends them
instead.

## Status

**Accuracy is not guaranteed for every title.** The ROM database covers 100
Model 1 and Model 2 sets, but a database entry only means the memory image can
be built -- it is not a claim that the game runs, let alone that it runs
correctly. Twelve have actually been exercised:

| Board | Tested |
| --- | --- |
| Model 1 | Virtua Racing, Virtua Fighter, Star Wars Arcade |
| Model 2 | Daytona USA (and Special Edition), Virtua Cop |
| Model 2A | Sega Rally Championship, Virtua Fighter 2 |
| Model 2B | Virtua Striker, Sonic Championship |
| Model 2C | The House of the Dead, Wave Runner |

Those boot, play, and render frames checked against MAME. Everything else is
untried: expect anything from a black screen to a subtly wrong one. If a game
is not on that list and it works, that is a happy accident rather than a
promise, and if it does not, that is the expected state rather than a surprise.

**No multiplayer.** The M2COMM link board is modelled only as far as a single
cabinet needs: the ring closes back on itself so the game's network check
passes and it runs standalone. Two instances cannot be linked, and the games
that were built around a linked pair -- the twin Daytona cabinets, Virtua
Striker's versus play -- run as one machine only.

## Roadmap

Roughly in the order it is likely to be worth doing.

- **More games, and the accuracy work that comes with them.** Twelve of a
  hundred sets have been run. Each new one tends to surface a real gap rather
  than a rendering nit -- Virtua Fighter 2's flying hair turned out to be an
  i960 burst-read bug, and Wave Runner's missing EEPROM was why it refused to
  boot at all.
- **Multiplayer.** The link board is simulated as a ring that closes on itself.
  Linking two instances over a socket, as MAME's `m2comm` does, would bring up
  twin Daytona and Virtua Striker's versus play.
- **Android.** The front end builds as a shared object with the activity glue
  in place, and the renderer and interface are rebuilt when the activity comes
  back so a game survives being backgrounded. It has never been built against
  an NDK or run on a device, and there is no touch control scheme yet.
- **Save states for Model 1.** The snapshot covers the Model 2 machine only;
  Virtua Racing, Virtua Fighter and Star Wars can save their battery-backed RAM
  but not their exact state.
- **The encrypted sets.** The 315-5881 implementation is in the tree and
  nothing calls it, so the games that need it -- Dynamite Cop, Zero Gunner and
  the rest -- do not run.
- **The later I/O board.** Wing War and Sega NetMerc use the `model1io2`
  board, which is not emulated, so they are left out of the ROM database's
  I/O firmware wiring.
- **Virtua Racing's region setting.** It reads the country from its EEPROM and
  boots in English, but changing it from the test menu does not survive a
  reboot: the game never writes the chip back. Worth tracing the firmware to
  find out whether hardware does.

## Layout

```
crates/i960        Intel i960KB              -- Model 2 main CPU
crates/v60         NEC V60                   -- Model 1 main CPU
crates/mb86233     Fujitsu MB86233 "TGP"     -- Model 1 and 2/2A geometry
crates/mb86235     Fujitsu MB86235 "TGPx4"   -- Model 2C geometry
crates/sharc       Analog Devices ADSP-21062 -- Model 2B geometry
crates/sega-crypt  Sega 315-5881 decryption
crates/tgpulse-core  the machine: memory maps, geometry, sound, save states
crates/tgpulse       the front end: window, renderer, interface, input, audio
tools/               the ROM database generator and a MAME comparison harness
```

`tgpulse-core` has no window, no GPU and no controller: a front end drives it a
frame at a time and reads back the framebuffer. That is what lets the debugger
and the comparison harness run the same machine headlessly.

## Accuracy

Behaviour is checked against MAME, which is the reference for these boards: the
memory maps are laid out in the same order as MAME's so the two can be read
side by side, and divergences are found by diffing instruction traces, work RAM
and rendered frames between the two. `tools/mamediff.sh` does the last of those.

The debugger is built to be driven by a program rather than typed at, so an
investigation is reproducible:

```sh
./target/release/tgpulse vf2 --debug -c "run 1400; geo; vertices"
```

Every line it prints is `kind key=value`, every command reports what it did,
and the machine is deterministic, so "run to here, poke, continue" replays
exactly.

## Building

A Cargo workspace, with no build system of its own and no vendored
dependencies. A stable Rust toolchain from [rustup](https://rustup.rs) is
enough; the binary lands in `target/release/tgpulse`.

**Linux.**

```sh
sudo apt install build-essential libudev-dev libasound2-dev   # Debian, Ubuntu
sudo pacman -S base-devel systemd-libs alsa-lib               # Arch
sudo dnf install gcc systemd-devel alsa-lib-devel             # Fedora

cargo build --release
```

`libudev` is for gamepad detection and ALSA is for audio. Rendering goes
through Vulkan where the driver offers it and GL otherwise, so a working
desktop driver is all that is needed beyond those.

**Windows.** Natively, with the MSVC toolchain and Visual Studio's C++ build
tools installed, there is nothing to configure:

```sh
cargo build --release
```

Cross-compiling from Linux with the GNU toolchain is also set up: the linker is
already named in `.cargo/config.toml`, so it needs only the target and mingw.

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64          # or: pacman -S mingw-w64-gcc
cargo build --release --target x86_64-pc-windows-gnu
```

That gives `target/x86_64-pc-windows-gnu/release/tgpulse.exe`, which wants the
same `roms/` directory beside it as everywhere else.

**Tests.**

```sh
cargo test --release
```

The processor tests assemble their own programs and need no ROM data.

[docs/BUILDING.md](docs/BUILDING.md) covers Android, which is packaged with
`cargo-apk` and has not yet been run on a device.

## Thanks

**The MAME project.** Decades of patient work went into documenting these
boards -- what each chip is, how the buses are wired, what the undocumented
registers do -- and published it where anyone could read it. That body of
knowledge is why a second implementation of this hardware is a weekend's
reading rather than a decade of reverse engineering, and it is the yardstick
this one measures itself against when a frame comes out wrong.

**ElSemi.** Nebula Model 2 ran these boards when nothing else could, and much
of what is now common knowledge about how the geometry pipeline and the
rasterizer behave was worked out there first. The widescreen mode in this
emulator exists because Nebula showed it could be done at all.

Neither is affiliated with this project, and nothing here should be taken as
speaking for them. Any inaccuracy is this program's own.

## License

MIT, in [LICENSE](LICENSE).

ROM images are copyrighted by their publishers and none are included here.

## Logging

Diagnostics are off by default and addressable per subsystem:

```sh
RUST_LOG=info ./target/release/tgpulse vf2
RUST_LOG=warn,geo=trace ./target/release/tgpulse vf2
```

Targets include `geo`, `fifo`, `io`, `sound`, `copro`, `nvram`, `backup`,
`comm`, `library` and `video`.
