# Build From Source

This guide covers local development builds for macOS and Linux. Most users
should prefer the published release archives from GitHub Releases.

## macOS

### Requirements

- macOS 14.4 or later.
- [Nix](https://nixos.org/download/) with flakes enabled.
- Full Xcode from the App Store, not only the command line tools.

The Nix development shell supplies Cargo, Rust, Python, and the manifest-signing
Python dependency while selecting Xcode's compiler and macOS SDK.

Select the full Xcode toolchain after installing it:

```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

### Build

```bash
git clone https://github.com/podocarp/meetlite.git
cd meetlite
nix develop --command bash scripts/build-macos-app.sh
```

The script uses locked dependencies, builds the CLI, GUI, and macOS capture
companion, derives the GUI bundle versions from the Cargo package version, and
applies ad-hoc signatures:

```text
dist/meetlite
dist/Meetlite.app
dist/MeetliteCapture.app
```

Open the GUI or run the CLI directly:

```bash
open dist/Meetlite.app
dist/meetlite record --duration 60
```

For a CLI-only build that does not create or sign app bundles:

```bash
nix develop --command cargo build --release --locked --bin meetlite
```

The release installer places the capture companion at
`~/Library/Application Support/Meetlite/MeetliteCapture.app`. A source build uses
the sibling `dist/MeetliteCapture.app` when no installed agent is present.

### Verify

```bash
nix develop --command cargo fmt --check
nix develop --command cargo test --locked
nix develop --command bash scripts/record-beep-test.sh
```

The beep test builds the app, rejects a generated silent recording as a negative
control, records system audio, and verifies the resulting `system.wav` contains
a tone from the beep fixture.

## Linux

Linux recording is intended to work across distributions. Debian 11 x86_64 is
the current tested checkpoint. CLI builds require Rust, `pkg-config`, ALSA
development headers, and the PulseAudio client library. GUI builds additionally
require X11, Wayland, `libxkbcommon`, and OpenGL/EGL development libraries.

```bash
sudo apt update
sudo apt install build-essential pkg-config libasound2-dev libpulse-dev ca-certificates libx11-dev libxcursor-dev libxi-dev libxrandr-dev libwayland-dev libxkbcommon-dev libgl1-mesa-dev libegl1-mesa-dev
```

Install a current stable Rust toolchain through [rustup](https://rustup.rs/),
then build and test with locked dependencies:

```bash
cargo test --locked
cargo build --release --locked --bin meetlite --bin meetlite-gui
```

For a CLI-only build, omit the GUI binary:

```bash
cargo build --release --locked --bin meetlite
```

Run either built interface:

```bash
target/release/meetlite devices
target/release/meetlite record --duration 60 --output ./meeting
target/release/meetlite-gui
```

The published Linux GUI tarball contains both executables because the GUI
locates the CLI beside itself. It is not an installer or self-contained bundle;
keep the binaries together and provide the runtime libraries listed in the
README.

Microphone capture uses CPAL. System audio first records the current default
PulseAudio monitor, which is the normal path on many Linux desktop sessions. This
also works on PipeWire desktops when the PulseAudio compatibility server is
running.

### ALSA fallback

If no PulseAudio-compatible server is running, configure an ALSA PCM capture
device in `~/.config/meetlite/config.json`. For the standard `snd-aloop` pairing,
audio played to `hw:Loopback,0,0` is captured from `hw:Loopback,1,0`:

```json
{
  "recording": {
    "system_device": "hw:Loopback,1,0"
  }
}
```

With a running PulseAudio server, verify the default-monitor path:

```bash
bash scripts/record-pulseaudio-test.sh
```

With `snd-aloop` loaded, verify ALSA loopback capture:

```bash
bash scripts/record-alsa-loopback-test.sh
```

Set `MEETLITE_LINUX_SYSTEM_DEVICE` when your loopback capture PCM differs from
`hw:Loopback,1,0`.

The test scripts require `python3`; the PulseAudio test also requires `pactl` and
`pacat`. They validate a mono 48 kHz `system.wav` and reject an unexpected
`audio.wav`. Run them inside `nix develop` if you use the Nix shell.

## Nix development shell

Nix is optional on Linux and required by the documented macOS build. It
provides the Rust toolchain and native development dependencies used by CI:

```bash
nix develop
cargo test
```

On macOS, build the companion app inside the shell:

```bash
bash scripts/build-macos-app.sh
```

## Platform notes

- macOS system audio uses a Core Audio process tap through `MeetliteCapture.app`.
- Linux system audio uses PulseAudio first, then an explicitly configured ALSA
  fallback.
- Windows system-audio capture is planned but not implemented yet.

Meetlite uses native CI runners and Cargo target triples for platform builds.
Feature flags should be reserved for optional capabilities, not operating-system
selection.
