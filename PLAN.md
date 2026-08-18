# Meetlite Plan

## Goal

Build a small Rust CLI for recording meetings to local, lossless audio files and
optionally transcribing them with an external OpenAI-compatible speech-to-text
service. Local model downloads, local inference, a GUI, Ollama, and bundled
LLM runtimes are explicitly out of scope.

The first supported platform is macOS. The capture architecture must make
Windows and Linux additions possible without changing the CLI, file format, or
remote API interfaces.

## MVP User Experience

```text
# Capture microphone and system audio into a local WAV file.
meetlite record

# Capture while uploading completed audio chunks to the configured STT API.
meetlite transcribe

# Transcribe an existing audio file with the configured STT API.
meetlite transcribe asd.wav

# Discover source names before recording.
meetlite devices
```

Default output layout:

```text
./meetlite-2026-08-18-143022/
  audio.wav
  metadata.json
  transcript.jsonl          # created by `transcribe`
  transcript.json           # finalized ordered transcript
```

`record` must work entirely offline. Network/API failures must never discard or
prevent creation of `audio.wav`.

## Decisions

- Language: Rust.
- Archive format: one mono, 48 kHz, 16-bit PCM WAV file.
- Internal audio format: mono, 48 kHz, `f32` PCM.
- macOS microphone capture: CPAL.
- macOS system-audio capture: native Core Audio process tap via `cidre`; no
  BlackHole, Loopback, FFmpeg, virtual device, or local model runtime.
- External STT: OpenAI-compatible multipart HTTP endpoint.
- External LLM configuration is included now, but summary generation is a
  later command, not an MVP requirement.
- Secrets: reference environment variables from config. Do not write API keys
  into meeting metadata or commit them to source control.

## What To Reuse From Meetily

Meetily is useful as a source of implementation patterns, not as a framework to
embed. Its active macOS system source is a Core Audio process tap, implemented
with `cidre`:

- `frontend/src-tauri/src/audio/capture/core_audio.rs`
  - Creates a global mono process tap.
  - Creates a private aggregate device containing that tap.
  - Reads PCM in a Core Audio IO callback and hands it to a ring buffer.
- `frontend/src-tauri/src/audio/stream.rs`
  - Uses CPAL callbacks for microphone samples and converts source sample
    formats to `f32`.
- `frontend/src-tauri/src/audio/pipeline.rs`
  - Demonstrates per-source normalization and asynchronous source buffering.

Keep the copied/adapted Core Audio code isolated in a macOS-only module. Retain
required upstream license notices when copying code.

Do not reuse Meetily's Tauri commands, recording state, model validation,
Whisper/Parakeet engines, VAD, transcript worker, model management, AAC/MP4
checkpoint saver, or FFmpeg integration.

Meetily's active mixer should not be copied unchanged: it uses 600 ms windows
despite comments claiming 50 ms, has stale mixing/ducking comments, and does
not drain remaining mixer windows at shutdown.

## Audio Pipeline

```text
CPAL microphone callback -----+--> normalize --> bounded source channel --+
                                                                    |       |
Core Audio process tap -------+--> normalize --> bounded source channel --+--> mixer --> WAV writer
```

### Source Handling

Each source emits `AudioFrame` values containing source kind, monotonic capture
time, sample rate, channel count, and samples. The source callback must only
copy/convert samples and push to a bounded non-blocking queue. It must not make
network calls, allocate unboundedly, or write files.

For every source:

1. Convert device samples to `f32` in the range `[-1.0, 1.0]`.
2. Downmix multichannel audio to mono by averaging channels.
3. Resample to 48 kHz only when the device does not deliver 48 kHz.
4. Attach a monotonic timestamp at capture ingress.

Request 48 kHz from devices first. Add a stateful `rubato` resampler only when
testing shows a selected device actually returns another rate.

### Mixer

The mixer owns short 20-50 ms timestamped windows for microphone and system
audio. It aligns windows by time, zero-fills a source that has no samples for a
window, applies fixed gains, sums the sources, and limits peaks.

Initial defaults:

```text
microphone gain: 1.0
system gain:     0.8
limiter:         scale the entire window only if its absolute peak exceeds 1.0
```

Make gains CLI options and save their values in `metadata.json`.

### Filtering

Required in the initial recorder:

- format conversion, downmixing, conditional resampling, fixed gain, and peak
  protection.

Recommended but optional behind a flag:

- a lightweight 80 Hz microphone high-pass filter to remove handling noise and
  low-frequency rumble.

Out of scope until real recordings demonstrate a need:

- RNNoise/noise suppression;
- EBU R128 loudness normalization;
- dynamic ducking or automatic gain control;
- VAD-based removal of silence;
- local diarization or local transcription.

These features optimize playback polish, local inference, or API cost. They
are not required to make a good archival file for a remote STT provider.

### Shutdown Correctness

Ctrl-C/SIGTERM must be treated as an orderly recording stop:

1. Stop microphone and system sources.
2. Stop accepting callback frames.
3. Drain source queues.
4. Flush resamplers and emit the final partial mixer window, zero-padding a
   missing source when necessary.
5. Drain and close the writer queue.
6. Finalize the WAV header and atomically write `metadata.json`.

Every stage needs an acknowledgement or join handle. Never use a fixed sleep to
guess that audio has been written.

## macOS Capture Spike

Before building the full CLI, prove the native system-audio path in a tiny
macOS-only binary.

Acceptance criteria:

1. Start a global mono Core Audio process tap using `cidre`.
2. Receive non-silent PCM while audio plays through the default system output.
3. Record ten seconds to a valid WAV file.
4. Verify that microphone and system audio are both present in a mixed file.
5. Verify clean Ctrl-C finalization and that the final half-second is retained.
6. Document exact macOS version, TCC permission prompt, and required bundle or
   usage-description settings.

Meetily's code notes that Core Audio Audio Capture permission is required on
macOS 14.4+. Its process-tap implementation captures global/default output;
device-selection behavior must be tested rather than assumed.

If the process-tap API is unavailable on a supported macOS version, fail with a
clear message. Do not silently fall back to a virtual device.

## CLI And Configuration

Use `clap` subcommands. Global options should include `--config`, `--output`,
`--verbose`, and `--json` for machine-readable progress/errors.

Configuration location:

```text
$XDG_CONFIG_HOME/meetlite/config.json
# macOS fallback: ~/.config/meetlite/config.json
```

Create the file with owner-only permissions where the platform supports them.
Support `MEETLITE_CONFIG` to override the path.

Initial schema:

```json
{
  "recording": {
    "sample_rate": 48000,
    "microphone_gain": 1.0,
    "system_gain": 0.8,
    "microphone_device": null,
    "system_device": null
  },
  "stt": {
    "base_url": "https://stt.example.com/v1",
    "model": "whisper-large-v3",
    "language": null,
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_STT_API_KEY"
    }
  },
  "llm": {
    "base_url": "https://llm.example.com/v1",
    "model": "gpt-4.1-mini",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_LLM_API_KEY"
    }
  }
}
```

Validate configuration before recording/transcribing. Support these auth modes:

- `none`;
- `bearer` plus `token_env`;
- `header` plus `header_name` and `value_env`.

Never log secret values. A future OS-keychain integration may provide a more
convenient secret store, but is not required for the MVP.

## Transcription

### Existing File

`meetlite transcribe asd.wav` uploads the specified file using:

```http
POST {stt.base_url}/audio/transcriptions
Authorization: Bearer $MEETLITE_STT_API_KEY
Content-Type: multipart/form-data

file=@asd.wav
model=whisper-large-v3
language=en                 # only when configured
response_format=verbose_json
```

Normalize provider responses into an internal transcript schema that preserves
full provider JSON in an optional `raw_response` field. At minimum preserve
text, segments/timestamps when supplied, provider, model, and source path.

The client must give clear errors for missing configuration, absent environment
variables, non-success HTTP responses, invalid JSON, timeouts, and oversized
files. Do not assume every OpenAI-compatible server supports every optional
field; make `response_format` configurable if interoperability demands it.

### Live Mode

`meetlite transcribe` records the canonical `audio.wav` exactly as `record`
does while also producing best-effort live transcript updates.

For the first implementation:

- rotate completed mixed audio into configurable 15-second temporary WAV
  chunks;
- upload chunks serially to preserve output order and avoid uncontrolled API
  concurrency;
- append each normalized result to `transcript.jsonl` immediately;
- print timestamped final text to stdout;
- write the finalized combined result to `transcript.json` at stop.

The canonical full WAV remains the source of truth. A failed live chunk is
recorded as pending/failed in metadata and can be retranscribed later.

Do not add VAD or partial-result streaming in the first release. Fixed windows
can miss context at boundaries; solve this later with configurable overlap and
timestamp-aware de-duplication, or by a final whole-file transcription pass.

## Project Layout

```text
meetlite/
  Cargo.toml
  src/
    main.rs
    cli.rs
    config.rs
    error.rs
    recording/
      mod.rs
      source.rs
      mixer.rs
      wav.rs
      metadata.rs
      microphone.rs
      macos_system.rs         # cfg(target_os = "macos")
    transcription/
      mod.rs
      openai_compatible.rs
      types.rs
  tests/
  PLAN.md
```

Initial dependencies:

```toml
anyhow = "1"
clap = { version = "4", features = ["derive"] }
cpal = "0.15"
crossbeam-channel = "0.5"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json", "multipart", "rustls-tls"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }

[target.'cfg(target_os = "macos")'.dependencies]
cidre = { git = "https://github.com/yury/cidre", rev = "PIN_A_TESTED_REVISION" }
```

Add `rubato` only if the capture spike proves resampling is needed. Write WAV
headers directly or use a small pure-Rust WAV crate; do not add FFmpeg.

## Milestones

### 1. Bootstrap And Config

- Create the Cargo binary crate and command skeleton.
- Implement config-path resolution, JSON validation, device listing, and safe
  config error messages.
- Add unit tests for config parsing and secret redaction.

### 2. macOS Audio Capture Spike

- Adapt Meetily's Core Audio process-tap lifecycle into `macos_system.rs`.
- Implement CPAL microphone capture.
- Confirm permissions, system output capture, and source shutdown manually.

### 3. Lossless Recorder

- Implement timestamped bounded channels, simple 48 kHz mixer, WAV writer,
  metadata, SIGINT handling, and drain acknowledgements.
- Implement `meetlite record` and test mic-only, system-only, and mixed files.

### 4. File Transcription

- Implement OpenAI-compatible multipart client and normalized transcript
  schema.
- Implement `meetlite transcribe FILE`.
- Test against at least one hosted Whisper-compatible API and one self-hosted
  compatible server, if available.

### 5. Live Transcription

- Add chunk rotation from the mixed recording pipeline.
- Implement ordered upload, JSONL checkpoints, failure persistence, and final
  transcript assembly.
- Implement `meetlite transcribe` with no input path.

#### 5a. Signed macOS Capture Agent (Complete)

- Recording coordination, microphone capture, mixing, local WAV writing, and
  STT uploads remain in the `meetlite` CLI.
- The macOS Core Audio process tap runs only in `MeetliteCapture.app`, a
  companion with a distinct stable bundle identifier and bound `Info.plist`.
  The release packaging supports a Developer ID identity; notarization remains
  a release-distribution task.
- The CLI starts the agent asynchronously through LaunchServices, authenticates
  a loopback-only IPC connection with a one-time random token, and receives
  timestamped PCM frames for the existing mixer.
- The capture agent is distributed beside the outer app at a stable path. The
  future setup command owns downloading, verification, and updates for that
  path.

#### 5b. Platform Agent Setup

- Add `meetlite setup` to install or update the platform-specific capture agent
  into a stable, versioned Meetlite cache directory.
- Download a signed release manifest and agent archive over HTTPS; verify the
  manifest signature and archive checksum before installation.
- Never execute an agent extracted to a random temporary directory. Keep prior
  verified versions for rollback if an update fails.
- Report the installed agent version, signing status, and the exact TCC
  permission users must grant. Keep the command no-op on platforms that do not
  require a companion agent.

### 6. Cross-Platform Capture

- Windows: microphone through CPAL; evaluate WASAPI loopback through a native
  implementation or a proven Rust binding.
- Linux: microphone through CPAL; add PipeWire/PulseAudio monitor-source
  support.
- Keep platform code behind `SystemAudioSource`; preserve the mixer, files,
  config, and STT interfaces unchanged.

### 7. Optional Summaries

- Add `meetlite summarize [TRANSCRIPT]` using the configured OpenAI-compatible
  LLM endpoint.
- Provide Markdown templates and write `summary.md` next to the transcript.
- Keep summary generation separate from recording and transcription failures.

## Verification

Automated tests:

- PCM conversion, mono downmixing, gain, limiting, and WAV header finalization.
- Mixer behavior with unequal source cadence, missing windows, and end-of-stream
  partial frames.
- Config validation and no-secret logging.
- Mock OpenAI-compatible STT requests/responses and failure handling.

Manual macOS tests:

- microphone-only recording;
- system-only recording while playing a known reference clip;
- mixed recording with simultaneous speech and system audio;
- selected Bluetooth/non-48-kHz microphone if available;
- Ctrl-C during active audio and during silence;
- denied and granted Audio Capture/microphone permissions;
- 30-minute recording with no unbounded memory growth or missing tail audio.

## Definition Of MVP Done

- On a supported macOS machine, `meetlite record` produces a valid local mixed
  WAV containing microphone and default system output without BlackHole,
  FFmpeg, a GUI, a local model, or a network connection.
- Stop and Ctrl-C preserve the final buffered audio and leave a valid WAV.
- `meetlite transcribe FILE` sends a WAV to a configured OpenAI-compatible STT
  endpoint and writes a normalized transcript.
- `meetlite transcribe` records and emits ordered best-effort live transcript
  chunks while preserving the complete local WAV.
- Secrets stay outside recording artifacts and logs.
