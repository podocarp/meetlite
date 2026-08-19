# Meetlite Agent Guide

## Project

Meetlite is a Rust CLI for local 48 kHz mono PCM WAV recording and optional
OpenAI-compatible transcription. macOS is the supported platform. Read
`PLAN.md` before changing capture, persistence, or transcription behavior.

## Architecture

- `src/recording/mod.rs` owns microphone capture, timestamped mixing, WAV
  output, metadata, and live-transcription sample delivery.
- `src/recording/macos_system.{rs,m}` owns the Core Audio process tap.
- `src/recording/macos_capture_agent.rs` hosts the signed capture-agent IPC.
- The CLI launches `MeetliteCapture.app` through LaunchServices. The agent
  authenticates to a loopback TCP listener with a one-time random token and
  streams timestamped PCM to the CLI mixer.
- `src/setup.rs` downloads and installs the agent at
  `~/Library/Application Support/Meetlite/MeetliteCapture.app`; it keeps the
  previous app as `.previous` for rollback.

Do not move mixing, WAV output, transcription, or application state into the
capture agent. Keep the process tap permission boundary small.

## macOS TCC

Audio Capture permission applies to `MeetliteCapture.app`, not a raw terminal
binary. The agent must be launched through LaunchServices and remain at a stable
installed path. Directly using the Core Audio bridge from the CLI produces
silent system-audio capture under TCC.

Current releases are ad-hoc signed. They are manifest-authenticated but are not
notarized and do not have stable Developer ID TCC behavior across updates.

## Release Process

`.github/workflows/release-macos.yml` runs on pushed `v*` tags. Its native
macOS matrix builds `aarch64` on `macos-14` and `x86_64` on `macos-13`. Each
build publishes `meetlite-macos-<architecture>.zip`,
`MeetliteCapture-macos-<architecture>.app.zip`, and
`MeetliteCapture-macos-<architecture>.manifest.json`. The workflow creates or
views the release before uploading with `--clobber`, so matrix jobs do not race
to create it.

- Required GitHub secret: `MEETLITE_MANIFEST_SIGNING_KEY`, a base64-encoded PEM
  Ed25519 private key.
- The corresponding public key is pinned in `src/setup.rs`.
- Canonical manifest payload:

```text
version=<version>
archive_url=<url>
archive_sha256=<lowercase hex sha256>
```

Do not change the payload format or public key without a migration plan; older
CLIs must continue to verify published manifests. Do not commit private keys.

Developer ID signing and notarization can be added later. If doing so, sign the
capture app before packaging it and retain manifest verification.

## Development

```bash
nix develop --command cargo fmt --check
nix develop --command cargo test
nix develop --command bash scripts/build-macos-app.sh
```

Use `scripts/record-beep-test.sh` for a manual system-audio smoke test. Run
`meetlite setup` against published releases to validate installer updates. Do
not use the archived Meetily application as a runtime dependency; it is a
reference implementation only.
