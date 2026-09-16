# Meetlite GUI Plan

## Status

The native GUI plan is implemented. Phases 0 through 3 are complete; this file
records the current architecture and the contracts that future changes must
preserve.

- [x] Phase 0: process, event, state, package, and test contracts
- [x] Phase 1: native GUI shell, child-process integration, and release packages
- [x] Phase 2: lifecycle-driven state, phase-aware cancellation, and partial output
- [x] Phase 3: onboarding, settings, and credential-safe configuration

## Current Architecture

Meetlite is one Rust crate with `meetlite` and `meetlite-gui` binaries. The GUI
is a native `egui`/`eframe` application using the `glow` backend; it does not use
Electron or a webview.

The GUI launches the existing CLI as a child process and does not duplicate
recording, transcription, summary, configuration, credential, or macOS
capture-agent logic. Packaged builds resolve the CLI relative to the GUI rather
than through `PATH`:

- Development: sibling `target/debug/meetlite`.
- Linux package: sibling `meetlite`.
- macOS app: `Meetlite.app/Contents/Resources/meetlite`.

Child stdout and stderr are read asynchronously so the render loop never blocks.
stdout is the typed NDJSON event stream. stderr is bounded diagnostic output and
does not drive normal state transitions. Detailed process, reducer, onboarding,
and protocol behavior is documented in `docs/gui.md`.

Recording directories contain `metadata.json` and aligned mono 48 kHz
`microphone.wav` and/or `system.wav`, never `audio.wav`. The command
`meetlite transcribe <RECORDING_DIRECTORY>` retranscribes source tracks
separately and merges segments labeled `You` and `Remote`.

macOS system capture remains exclusively in the signed
`MeetliteCapture.app`, launched through LaunchServices. Audio Capture TCC
permission belongs to that stable app bundle, not the GUI or terminal CLI.

## Process And Event Contract

The GUI starts a recording session with:

```text
meetlite --json start
```

When `--json` is set, stdout contains JSON only. Top-level failures emit one
final redacted `error` event before a nonzero exit. Human-readable CLI behavior
is unchanged without `--json`.

Lifecycle events are:

```json
{"type":"lifecycle","phase":"recording_started","output_dir":"...","summary_enabled":true}
{"type":"lifecycle","phase":"recording_stopped","output_dir":"..."}
{"type":"lifecycle","phase":"processing_started","output_dir":"..."}
{"type":"lifecycle","phase":"summarizing_started","transcript_path":"..."}
{"type":"error","message":"..."}
```

Existing event names remain compatible:

```text
transcription_chunk
transcription_chunk_failed
transcription_completed
summary_delta
summary_completed
```

Credentials, authorization values, and capture-agent tokens must never appear
in UI output, logs, events, stderr diagnostics, or command-line arguments.

## UX And State Contract

The GUI uses a compact native utility window with a primary control, concise
status, settings access while idle, and scrollable chronological output.
Lifecycle events, not child liveness or stderr text, determine active phases.

| State | Primary control | Click behavior | CLI action |
| --- | --- | --- | --- |
| Ready | Start | Start recording | Spawn `meetlite --json start` |
| Recording | Red recording symbol | Stop recording | First SIGINT |
| Processing | Spinner | Stop processing and summarize partial transcript | Second SIGINT |
| Summarizing | Spinner | Stop summary | Third SIGINT |
| Complete | Reset | Clear GUI session | None |
| Stopped | Reset | Clear GUI session | None |
| Failed | Reset | Clear GUI session | None |
| SetupRequired | Open setup | Show onboarding | None |

- Recording tooltip: `Stop recording`.
- Processing tooltip: `Stop processing and summarize partial transcript`.
- Summarizing tooltip: `Stop summary`.
- Repeat clicks are disabled while signal delivery is pending.
- The second interrupt preserves completed checkpoints, writes a partial
  transcript, skips remaining transcription, and permits partial summarization.
- The third interrupt stops summary generation. If the child does not exit
  within the cancellation grace period, the GUI terminates it and reports
  Stopped.
- Reset clears only GUI display and session state. It never deletes recordings,
  WAV files, transcripts, or summaries.

## Configuration And Credential Contract

At startup, the GUI probes configuration with `meetlite --json config status`.
Missing or unusable configuration opens onboarding; idle users can reopen the
same minimal form from Settings.

Configuration is applied through:

```text
meetlite --json config apply --stdin-json
```

Secrets are masked in the GUI and sent only through child stdin. They are never
passed in argv. A managed saved key is represented only by the literal
non-secret mask `********`: unchanged preserves it, blank removes it, and other
text replaces it. Copy and cut are blocked for key fields while paste remains
available. macOS credentials remain in the single Keychain item
`Meetlite / Meetlite API Credentials`, with distinct `stt` and `llm` values.
Status and save events expose only redacted metadata.

## Package And Release Contract

macOS build output is:

```text
dist/
  meetlite
  MeetliteCapture.app/
  Meetlite.app/
    Contents/
      Info.plist
      MacOS/meetlite-gui
      Resources/meetlite
      Resources/MeetliteCapture.app/
```

`scripts/build-macos-app.sh` derives GUI and capture-app
`CFBundleShortVersionString` and `CFBundleVersion` values from the Cargo package
version, or the matching release version supplied by CI. Nested capture code is
signed before the GUI app, and both are verified with
`codesign --verify --deep --strict`. The resource CLI and capture app remain
siblings so capture-agent lookup and the TCC owner do not change.

Linux packages place `meetlite` and `meetlite-gui` in the same extracted
directory. The GUI archive is not self-contained; users run `./meetlite-gui`
with compatible glibc, audio, X11 or Wayland, keyboard, and OpenGL/EGL runtime
libraries installed.

Releases retain the existing CLI archive, standalone capture-app archive, and
signed capture manifest for legacy clients, CLI-only installers, and
capture-agent updaters. GUI archives are additional assets and do not replace
those distributions. Preserve the published asset names documented in
`AGENTS.md`.

The capture manifest payload remains exactly:

```text
version=<version>
archive_url=<url>
archive_sha256=<lowercase hex sha256>
```

Do not change that payload, its signing process, or signing material without a
compatible migration for existing clients.

## Verification Contract

Automated coverage includes reducer transitions, lifecycle ordering, duplicate
clicks, natural completion, crashes, structured failures, partial transcripts,
configuration handling, missing executables, signal ordering, cancellation
timeouts, and NDJSON parsing.

Use the repository quality gates:

```bash
nix develop --command cargo fmt --check
nix develop --command cargo test
nix develop --command cargo build --release --locked
nix develop --command bash scripts/build-macos-app.sh
git diff --check
```

Release review must also inspect executable and archive layouts, generated plist
versions, macOS signatures, preserved legacy assets, and the unchanged capture
manifest payload.
