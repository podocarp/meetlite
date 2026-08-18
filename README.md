# Meetlite

Meetlite is a lightweight Rust meeting-recorder CLI. The current macOS spike
captures the default microphone and system audio into a single local WAV file.
It does not use a virtual audio device, FFmpeg, local AI models, or a GUI.

## Requirements

- macOS 14.4 or later for Core Audio process taps.
- Full Xcode, not only Command Line Tools.
- Nix with flakes enabled.

Select Xcode after installing it:

```bash
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

The first recording may prompt for microphone and Audio Capture permission.
Grant both permissions. If permission was previously denied, enable Meetlite's
terminal process under **System Settings > Privacy & Security**.

## Build

Enter the development shell and build the debug binary:

```bash
nix develop
cargo build
```

The binary is at `target/debug/meetlite`. Run it through Nix without entering a
shell with:

```bash
nix develop --command cargo run -- --help
```

Run the checks:

```bash
nix develop --command cargo fmt --check
nix develop --command cargo test
```

## Configuration

Create the default configuration file:

```bash
nix develop --command cargo run -- config init
```

The default path is `~/.config/meetlite/config.json`. Print the effective path
with:

```bash
nix develop --command cargo run -- config path
```

Set `MEETLITE_CONFIG` or pass `--config PATH` to use another file.

`record` works without a config file. Add an `stt` section to transcribe files.
Reference API secrets by environment variable, not directly in the JSON file:

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
    "transcription_path": "/audio/transcriptions",
    "model": "whisper-large-v3",
    "language": null,
    "response_format": "verbose_json",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_STT_API_KEY"
    }
  }
}
```

Then set the key in the shell that runs Meetlite:

```bash
export MEETLITE_STT_API_KEY='...'
```

Supported auth configurations are `none`, `bearer` with `token_env`, and
`header` with `header_name` and `value_env`.

## Recording

List available microphone devices:

```bash
nix develop --command cargo run -- devices
```

Record until Ctrl-C:

```bash
nix develop --command cargo run -- record
```

Record a fixed duration into a chosen output directory:

```bash
nix develop --command cargo run -- record --duration 60 --output ./meeting
```

Record one source for microphone-only or system-only verification:

```bash
nix develop --command cargo run -- record --no-system-audio --duration 10 --output ./mic-only
nix develop --command cargo run -- record --no-microphone --duration 10 --output ./system-only
```

Override the configured mixing gains for one recording:

```bash
nix develop --command cargo run -- record --microphone-gain 0.9 --system-gain 0.7
```

The output directory is created by Meetlite and contains `audio.wav`, a mono
48 kHz 16-bit PCM WAV, plus `metadata.json` with source settings and dropped
frame counts. When `--output` is omitted, Meetlite creates a timestamped
`meetlite-...` directory in the current working directory.

## Smoke Test

`beep-02.wav` is a short fixture for manually testing system-audio capture. The
script builds the app, starts an eight-second recording, waits two seconds for
capture startup, then plays the beep repeatedly:

```bash
bash scripts/record-beep-test.sh
```

It prints the temporary recording path and inspects the resulting WAV with
`afinfo`. Listen to the output file to confirm the beep is present.

## Transcription

Transcribe an existing WAV using the configured provider:

```bash
nix develop --command cargo run -- transcribe ./meeting/audio.wav
```

Meetlite writes the normalized result to `transcript.json` beside the input, or
to the directory passed with `--output`. It preserves the original provider
JSON under `raw_response` for provider-specific fields.

For a local `whisper-cpp` server, the Nix development shell includes
`whisper-server`. Start it with a downloaded compatible GGML model:

```bash
whisper-server --model /path/to/ggml-base.en.bin --port 8080
```

Configure its native endpoint without an API key:

```json
{
  "stt": {
    "base_url": "http://127.0.0.1:8080",
    "transcription_path": "/inference",
    "model": "local-whisper-cpp",
    "language": "en",
    "response_format": "verbose_json",
    "auth": { "type": "none" }
  }
}
```

The default `transcription_path` is `/audio/transcriptions`, which is the
OpenAI-compatible endpoint. `whisper-server` is a local test and development
option; it requires a separately supplied model file.

## Current Status

Implemented:

- macOS default microphone capture through CPAL.
- macOS global system-audio capture through a native Core Audio process tap.
- Timestamp-aligned, bounded 20 ms mixing windows with fixed gains and peak
  limiting.
- Mixed and single-source lossless WAV recording with acknowledged source
  shutdown and metadata artifacts.
- JSON configuration initialization and validation.
- File transcription through OpenAI-compatible multipart STT uploads, including
  normalized `transcript.json` artifacts.

Not implemented yet:

- Live transcription and LLM summaries.
- Conditional resampling for devices that do not deliver 48 kHz, and
  cross-platform system capture.
