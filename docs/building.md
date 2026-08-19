# Build From Source

The published macOS CLI does not require Xcode, Rust, or Nix. This guide builds
the complete macOS suite from source: the `meetlite` CLI and its
`MeetliteCapture.app` system-audio companion.

## Requirements

- Rust and cargo installed.
- macOS 14.4 or later.
- *Full* Xcode from the app store, not just command line tools.
- A current stable Rust toolchain from [rustup](https://rustup.rs/).

Select the full Xcode toolchain after installing it from the appstore:
```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

## Build

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

## Verify

```bash
cargo fmt --check
cargo test
bash scripts/record-beep-test.sh
```

The beep test builds the app, records system audio, and inspects the resulting
WAV file.

## Nix

Nix is optional. When it is available, it provides a pinned Rust development
environment and `whisper-server` for local transcription testing:

```bash
nix develop
bash scripts/build-macos-app.sh
```

## Other Platforms

Windows and Linux system-audio capture are not supported yet. Their release jobs
will be added when their native capture implementations are available.

Meetlite uses native CI runners and Cargo target triples for platform builds.
This keeps OS-specific capture code out of user-facing feature flags. Feature
flags should be reserved for optional capabilities, such as hardware
acceleration, rather than operating-system selection.
