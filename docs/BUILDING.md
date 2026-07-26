# Building

TGPulse is a Cargo workspace. A native build needs nothing but a Rust
toolchain:

```sh
cargo build --release
./target/release/tgpulse --list
```

The binary is `target/release/tgpulse`. ROMs are read from `roms/` next to the
working directory; `--roms <dir>` points it elsewhere.

## Linux

```sh
cargo build --release --target x86_64-unknown-linux-gnu
```

Needs the usual desktop development packages: a Vulkan (or GL) driver,
`libudev` for gamepad detection, and ALSA or PulseAudio headers for audio. On
a Debian-derived distribution:

```sh
sudo apt install build-essential libudev-dev libasound2-dev
```

## Windows

Cross-compiling from Linux with the GNU toolchain:

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64          # or: pacman -S mingw-w64-gcc
cargo build --release --target x86_64-pc-windows-gnu
```

The linker is already named in `.cargo/config.toml`. Building on Windows
itself with the MSVC toolchain needs no configuration:

```sh
cargo build --release --target x86_64-pc-windows-msvc
```

## Android

The front end is also built as a shared object whose `android_main` the
activity calls. Packaging is done by `cargo-apk`:

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi
cargo install cargo-apk
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/<version>
cargo apk build --release -p tgpulse
```

The package name, SDK levels and activity settings are in the
`[package.metadata.android]` section of `crates/tgpulse/Cargo.toml`.

ROMs go in the application's external data directory, under `roms/`:

```sh
adb shell mkdir -p /sdcard/Android/data/org.tgpulse.emulator/files/roms
adb push vf2.zip /sdcard/Android/data/org.tgpulse.emulator/files/roms/
```

Battery-backed RAM and save states are written alongside them.

Two things to know about the Android target. The renderer and the interface
are rebuilt whenever the activity is resumed, because the drawable surface
does not survive being backgrounded; the emulated machine does survive, so a
game continues where it left off. And gamepad support goes through winit
rather than `gilrs`, which has no Android backend -- a controller paired with
the device works, but the library's own device enumeration reports nothing.

## Tests

```sh
cargo test --release
```

The CPU tests run from hand-assembled programs and need no ROM data. The
emulator's behaviour against real games is checked with the debugger, which is
scriptable:

```sh
./target/release/tgpulse vf2 --debug -c "run 1400; vertices; screenshot /tmp/f.ppm"
```
