# GUI Architecture

Meetlite's implemented GUI is a native `egui`/`eframe` application that wraps
the existing CLI process. This document describes its current process,
protocol, state, packaging, and verification architecture.

## Process Model

`meetlite-gui` is a native `egui`/`eframe` companion binary. It does not link to or duplicate the recording, transcription, summary, configuration, credential, or macOS capture-agent implementations. For each session it launches one sibling CLI process as:

```text
meetlite --json start
```

Packaged builds resolve the CLI relative to the GUI executable and never through `PATH`:

- Development: sibling `target/debug/meetlite`.
- Linux package: sibling `meetlite`.
- macOS app: `Meetlite.app/Contents/Resources/meetlite` relative to `Contents/MacOS/meetlite-gui`.

The child has piped stdout and stderr. Independent reader threads consume both streams and send messages over a channel to the GUI. Process waiting and signal delivery also run outside the render thread. The GUI drains only currently available channel messages during a frame and never waits for child output, process exit, or signal completion.

stdout is UTF-8 newline-delimited JSON with one typed event per line. In `--json` mode every stdout line must be JSON; status, instructions, panics, and diagnostics must not be written there. stderr is retained as bounded diagnostic text and may be shown with a failure, but it does not drive normal state transitions. Credentials and other secrets must never be included in either stream.

The controller sends one SIGINT per accepted stop action. It disables the primary control until signal delivery completes. The first interrupt stops recording, the second stops remaining transcription and permits partial summarization, and the third stops summary generation. If a child does not exit after summary cancellation within the configured grace period, the controller terminates it and reports a cancellation timeout to the reducer.

At startup, the GUI asynchronously runs `meetlite --json config status`. A valid unusable result opens first-run onboarding; a probe failure opens a retry screen without presenting a default form. A persistent Settings control in the app top bar opens the same one-page form whenever no recording session is active. Saves run `meetlite --json config apply --stdin-json`; secrets are masked in the form, sent only through the child stdin pipe, and never placed in argv or events.

## Event Protocol

Every event is a JSON object with a string `type`. Paths are strings in the platform-native display form. Readers ignore unknown object fields for forward compatibility. An unknown `type`, malformed JSON line, missing required field, or non-object stdout value is a protocol error and makes the session fail.

Lifecycle events use this shape:

```json
{"type":"lifecycle","phase":"recording_started","output_dir":"...","summary_enabled":true}
{"type":"lifecycle","phase":"recording_stopped","output_dir":"..."}
{"type":"lifecycle","phase":"processing_started","output_dir":"..."}
{"type":"lifecycle","phase":"summarizing_started","transcript_path":"..."}
```

Their ordering for a successful `start` session is:

```text
recording_started
(transcription_chunk | transcription_chunk_failed)*
recording_stopped
processing_started
(transcription_chunk | transcription_chunk_failed)*
transcription_completed
summarizing_started
summary_delta*
summary_completed
child exit 0
```

`recording_started` is emitted only after all requested capture sources start successfully. Its `summary_enabled` field identifies the actual mode used by that child; older children may omit it. `recording_stopped` is emitted after capture stops and recording artifacts are finalized. `processing_started` marks transcription queue draining and can immediately follow `recording_stopped`. `summarizing_started` is emitted before summary provider work begins.

Existing event names remain compatible. New CLI versions identify the independently transcribed source as `microphone` or `system`; older versions may omit `source`:

```json
{"type":"transcription_chunk","chunk_index":0,"source":"microphone","start_seconds":0.0,"text":"..."}
{"type":"transcription_chunk_failed","chunk_index":0,"source":"system","start_seconds":0.0,"error":"..."}
{"type":"transcription_completed","transcript_path":"...","transcript":{}}
{"type":"summary_delta","text":"..."}
{"type":"summary_completed","summary_path":"...","model":"...","summary":"..."}
```

A top-level command failure in JSON mode emits one final event before a nonzero exit:

```json
{"type":"error","message":"..."}
```

The message is suitable for display and redacted of API keys, authorization values, capture-agent tokens, and credential payloads. The GUI treats `error` as authoritative failure state and uses stderr only as supplemental diagnostics. Human-readable behavior without `--json` remains unchanged.

## Session Reducer

`src/gui/session.rs` is the pure reducer used by the application and unit tests. UI callbacks, stream readers, timers, and process waiters convert external activity into `SessionEvent` values. The reducer owns state and returns optional side effects for the process controller; it performs no I/O.

| Current state | Input | Next state | Effect |
| --- | --- | --- | --- |
| SetupRequired | Primary control | SetupRequired | Open setup |
| SetupRequired | Setup completed | Ready | None |
| Ready | Primary control | Ready, action pending | Spawn child |
| Ready | `recording_started` | Recording | None |
| Ready | Launch failure | Failed | None |
| Recording | Primary control | Recording, action pending | Send first SIGINT |
| Recording | `recording_stopped` or `processing_started` | Processing | None |
| Processing | Primary control | Processing, action pending | Send second SIGINT |
| Processing | `summarizing_started` | Summarizing | None |
| Processing | Successful exit after transcription when summaries are disabled | Complete | None |
| Summarizing | Primary control | Summarizing, action pending | Send third SIGINT |
| Summarizing | `summary_completed` | Summarizing, completion observed | None |
| Summarizing | Successful exit after completion | Complete | None |
| Summarizing | Exit after third SIGINT | Stopped | None |
| Summarizing | Cancellation timeout after third SIGINT | Stopped | Terminate child |
| Any active state | `error`, invalid protocol, launch failure, or unexpected exit | Failed | None |
| Complete, Stopped, Failed | Primary control | Ready | None |

Repeated primary clicks while an action is pending have no effect. Signal-delivery completion clears the pending flag. A process exit is Complete only when `summary_completed` was observed and the exit status is successful. An exit after requested summary cancellation is Stopped. Every other exit before terminal completion is Failed, even if its status is successful. Reset discards only reducer and displayed session data; it never removes artifacts.

Lifecycle events, not process liveness or stderr text, determine Recording, Processing, and Summarizing. Events that cannot validly advance the current state are ignored unless event parsing itself failed.

## Package Layouts

The existing terminal distributions remain available. The curl installer uses
the GUI assets by default so the sibling CLI is installed with the GUI;
`--cli-only` selects the legacy CLI archives.

macOS build output:

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

The nested capture app is signed before `Meetlite.app`; both are verified with `codesign --verify --deep --strict`. `Resources/meetlite` and `Resources/MeetliteCapture.app` remain siblings so the existing CLI lookup can launch the signed capture app without changing the Audio Capture TCC owner. Releases retain `meetlite-macos-aarch64.zip`, `MeetliteCapture-macos-aarch64.app.zip`, and the signed capture manifest, and add `Meetlite-macos-aarch64.app.zip`. After extracting the GUI archive, users can move `Meetlite.app` to `/Applications` and open it normally. The capture manifest payload and signing inputs do not change.

The Linux GUI asset is `meetlite-gui-linux-x86_64.tar.gz`; it places `meetlite-gui` and `meetlite` in the same directory because executable lookup is relative. It is not a self-contained application bundle and does not install a desktop entry or icon. Users must keep both files together and supply a graphical X11 or Wayland session, compatible glibc, audio libraries, `libxkbcommon`, and OpenGL/EGL runtime libraries before running `./meetlite-gui`.

## Verification

Pure reducer tests cover setup, launch, lifecycle transitions, duplicate clicks,
the stop actions, natural completion with and without summaries, structured
failure, child crashes, cancellation timeout, and reset. Event parser and
process-controller tests cover valid and invalid NDJSON, asynchronous streams,
signal ordering, missing executables, exit classification, and timeout
escalation.

CLI tests cover lifecycle ordering, redacted failures, and partial transcript
persistence. The macOS build script checks both app signatures, and release
checks inspect executable and archive layouts while preserving the capture
manifest payload.
