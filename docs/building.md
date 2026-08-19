# Build From Source

The published macOS CLI does not require Xcode, Rust, or Nix. This guide covers
the complete macOS suite and the Linux CLI.

## macOS

### Requirements

- Rust and cargo installed.
- macOS 14.4 or later.
- *Full* Xcode from the app store, not just command line tools.
- A current stable Rust toolchain from [rustup](https://rustup.rs/).

Select the full Xcode toolchain after installing it from the appstore:
```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

### Build

```bash
git clone https://github.com/podocarp/meetlite.git
cd meetlite
bash scripts/build-macos-app.sh
```

The script builds both executables, embeds their matching macOS plist files,
and applies an ad-hoc signature. Its output is:

```text
dist/meetlite
dist/MeetliteCapture.app
```

Use the direct CLI while developing:

```bash
dist/meetlite record --duration 60
```

`meetlite setup` is not part of the build. It downloads a published capture
agent for a release CLI. The source CLI finds its sibling
`dist/MeetliteCapture.app` when no installed agent is present.

### Verify

```bash
cargo fmt --check
cargo test
bash scripts/record-beep-test.sh
```

The beep test builds the app, records system audio, and inspects the resulting
WAV file.

### Nix

Nix is optional. When it is available, it provides a pinned Rust development
environment and `whisper-server` for local transcription testing:

```bash
nix develop
bash scripts/build-macos-app.sh
```

## Linux

Linux builds require Rust, `pkg-config`, ALSA development headers, and the
PulseAudio client library. On Debian or Ubuntu:

```bash
sudo apt install build-essential pkg-config libasound2-dev libpulse-dev
```

Install a current stable Rust toolchain through [rustup](https://rustup.rs/).

The Nix shell provides those dependencies on x86_64 Linux:

```bash
nix develop
cargo test
cargo build --release
```

Run the built CLI directly:

```bash
target/release/meetlite devices
target/release/meetlite record --duration 60 --output ./meeting
```

Microphone capture uses CPAL. System audio first records the current default
PulseAudio monitor. This also works on PipeWire desktops when the PulseAudio
compatibility server is running. No recording configuration is required for
that normal desktop path.

### ALSA Fallback

If no PulseAudio-compatible server is running, configure an ALSA PCM capture
device in `~/.config/meetlite/config.json`. For the standard `snd-aloop`
pairing, audio played to `hw:Loopback,0,0` is captured from
`hw:Loopback,1,0`:

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

With the `snd-aloop` module loaded, verify a real system-audio capture using
the paired default loopback devices when PulseAudio is unavailable:

```bash
bash scripts/record-alsa-loopback-test.sh
```

Set `MEETLITE_LINUX_SYSTEM_DEVICE` when your loopback capture PCM differs from
`hw:Loopback,1,0`.

The test scripts require `python3`; the PulseAudio test also requires `pactl`
and `pacat`. Run them inside `nix develop` when using the Nix shell.

Windows system-audio capture is not supported yet.

Meetlite uses native CI runners and Cargo target triples for platform builds.
This keeps OS-specific capture code out of user-facing feature flags. Feature
flags should be reserved for optional capabilities, such as hardware
acceleration, rather than operating-system selection.
