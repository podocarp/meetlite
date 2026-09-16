# Meetlite Agent Guide

## Development

- This is a single Rust crate with the `meetlite` CLI and `meetlite-gui` native
  GUI binaries. Use the Nix shell commands because macOS builds require Xcode's
  clang and SDK settings from `flake.nix`:

  ```bash
  nix develop --command cargo fmt --check
  nix develop --command cargo test
  nix develop --command cargo test transcription::tests::test_name
  ```

- Build the macOS CLI, GUI, and signed capture companion with
  `nix develop --command bash scripts/build-macos-app.sh`. It writes
  `dist/meetlite`, `dist/Meetlite.app`, and `dist/MeetliteCapture.app`. The
  script derives GUI and capture-app bundle versions from Cargo metadata;
  release CI passes the tag and rejects it unless it matches the Cargo package
  version.
- Use `scripts/record-beep-test.sh` only for a macOS system-audio smoke test;
  it plays audio and requires capture permission. Linux manual capture checks
  are `scripts/record-pulseaudio-test.sh` and
  `scripts/record-alsa-loopback-test.sh`.

## Recording And Transcription

- `src/pipeline.rs` selects recording, transcription, and summary flows.
  `src/recording/mod.rs` owns timestamp alignment, source WAV output, and
  delivery to live transcription; do not move these responsibilities into a
  platform capture implementation.
- Recordings preserve each enabled source as aligned 48 kHz mono PCM:
  `microphone.wav` is local speech and `system.wav` is remote/system audio.
  Never create `audio.wav`; users may mix the aligned tracks externally.
  `transcribe <RECORDING_DIRECTORY>` performs paired retranscription and merges
  segments labeled `You` and `Remote`; it cannot identify individual remote
  participants.
- macOS system capture must run in `MeetliteCapture.app`, launched through
  LaunchServices by `src/recording/macos_capture_agent.rs`. Audio Capture TCC
  permission belongs to that stable app bundle, not the terminal CLI. Keep the
  agent limited to the Core Audio tap and authenticated loopback PCM IPC.
- The recorder first uses the installed capture app at
  `~/Library/Application Support/Meetlite/MeetliteCapture.app`, then falls back
  to a sibling `MeetliteCapture.app` beside the CLI. `scripts/install.sh`
  maintains the installed app and its `.previous` rollback copy.

## Releases

- Pushing a `v*` tag runs native macOS and Linux release workflows. Linux CI is
  the authoritative command order: `cargo fmt --check`, `cargo test --locked`,
  then `cargo build --release --locked` inside `nix develop`.
- macOS publishes `meetlite-macos-aarch64.zip`,
  `Meetlite-macos-aarch64.app.zip`,
  `MeetliteCapture-macos-aarch64.app.zip`, and the signed capture manifest.
  Linux publishes `meetlite-linux-x86_64.tar.gz` and
  `meetlite-gui-linux-x86_64.tar.gz`; the GUI tarball contains sibling
  `meetlite` and `meetlite-gui` binaries and is not self-contained. Preserve
  the existing CLI, capture-app, and signed manifest assets and names for legacy
  clients, `--cli-only` installs, and capture-agent updaters. The default
  installer installs both CLI and GUI.
- The capture manifest payload must remain exactly:

  ```text
  version=<version>
  archive_url=<url>
  archive_sha256=<lowercase hex sha256>
  ```

- macOS manifest generation requires the base64 PEM Ed25519 secret
  `MEETLITE_MANIFEST_SIGNING_KEY`. Never commit signing material. The capture
  app is ad-hoc signed; preserve signing before packaging if changing that flow.
