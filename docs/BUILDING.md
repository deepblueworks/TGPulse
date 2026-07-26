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

The linker has to be named somewhere Cargo reads, either in the checkout's
`.cargo/config.toml` or in `$CARGO_HOME/config.toml`:

```toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"
```

Building on Windows itself with the MSVC toolchain needs no configuration:

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
cargo apk build --release -p tgpulse --lib
```

`--lib` is not optional. The package builds both a binary and the shared
object, and `cargo-apk` panics ("Bin is not compatible with Cdylib") if it is
left to walk onto the binary after it has finished packaging the library.

A release build also has to be signed, either from
`[package.metadata.android.signing.release]` or from the environment:

```sh
export CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=...
```

The `oboe` audio backend links `libc++_shared.so`, which is not present on a
device and has to be packaged. That is what `runtime_libs = "."` in the
manifest is for: it expects a directory per ABI next to `crates/tgpulse`,
holding the copy from the NDK.

```sh
SYSROOT=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib
mkdir -p crates/tgpulse/arm64-v8a crates/tgpulse/armeabi-v7a
cp $SYSROOT/aarch64-linux-android/libc++_shared.so crates/tgpulse/arm64-v8a/
cp $SYSROOT/arm-linux-androideabi/libc++_shared.so crates/tgpulse/armeabi-v7a/
```

The package name, SDK levels and activity settings are in the
`[package.metadata.android]` section of `crates/tgpulse/Cargo.toml`.

ROMs go in the application's external data directory, under `roms/`:

```sh
adb shell mkdir -p /sdcard/Android/data/org.tgpulse.emulator/files/roms
adb push vf2.zip /sdcard/Android/data/org.tgpulse.emulator/files/roms/
```

Battery-backed RAM and save states are written alongside them.

Three things to know about the Android target.

The renderer and the interface are rebuilt whenever the activity is resumed,
because the drawable surface does not survive being backgrounded; the emulated
machine does survive, so a game continues where it left off.

The interface is not the desktop one. ImGui's windows assume a pointer that
can hover and a keyboard that can type, so on a handset they are replaced
wholesale by `crate::touch`: on-screen controls chosen to match the loaded
cabinet, and a menu of the things a phone player needs. Both are drawn through
ImGui's draw list rather than its widgets, which is why the renderer is
unchanged. The settings, rebinding and debugger panels are desktop-only --
they want a keyboard, and `winit`'s Android backend cannot raise the soft one
(`set_ime_allowed` is a stub there).

Gamepad support does not go through `gilrs`, which has no Android backend.
A controller arrives as activity key events instead: winit maps the d-pad onto
the arrow keys and hands the rest back as raw `KEYCODE_BUTTON_*` values, which
`android_pad_button` in `app.rs` translates into the same `gilrs::Button`
names the bindings already use. The left stick is recovered from the motion
events winit reports as touches -- see `App::on_touch` for how a stick is told
apart from a finger. Triggers and the hat are only seen when the pad also
sends them as key events, which most do.

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
