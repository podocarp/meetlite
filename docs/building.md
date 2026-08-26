# Build From Source

This guide covers local development builds for macOS and Linux. Most users
should prefer the published release archives from GitHub Releases.

## macOS

### Requirements

- macOS 14.4 or later.
- A current stable Rust toolchain from [rustup](https://rustup.rs/).
- Full Xcode from the App Store, not only the command line tools.

Select the full Xcode toolchain after installing it:

```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

### Build

```bash
git clone https://github.com/podocarp/meetlite.git
cd meetlite
bash scripts/build-macos-app.sh
```

The script builds the CLI and the macOS capture companion, embeds their plist
files, and applies an ad-hoc signature:

```text
dist/meetlite
dist/MeetliteCapture.app
```

Run the development CLI directly:

```bash
dist/meetlite record --duration 60
```

The release installer places the capture companion at
`~/Library/Application Support/Meetlite/MeetliteCapture.app`. A source build uses
the sibling `dist/MeetliteCapture.app` when no installed agent is present.

### Verify

```bash
cargo fmt --check
cargo test
bash scripts/record-beep-test.sh
```

The beep test builds the app, rejects a generated silent recording as a negative
control, records system audio, and verifies the resulting WAV contains a tone
from the beep fixture.

## Linux

Linux recording is intended to work across distributions. Debian 11 x86_64 is the
current tested checkpoint. Linux builds require Rust, `pkg-config`, ALSA
development headers, and the PulseAudio client library.

```bash
sudo apt update
sudo apt install build-essential pkg-config libasound2-dev libpulse-dev ca-certificates
```

Install a current stable Rust toolchain through [rustup](https://rustup.rs/),
then build:

```bash
cargo test
cargo build --release
```

Run the built CLI:

```bash
target/release/meetlite devices
target/release/meetlite record --duration 60 --output ./meeting
```

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
`pacat`. Run them inside `nix develop` if you use the Nix shell.

## Nix development shell

Nix is optional. It provides a pinned Rust development environment and
`whisper-server` for local transcription testing:

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
