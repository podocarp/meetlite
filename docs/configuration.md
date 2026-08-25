# Configuration

Meetlite reads JSON configuration from `~/.config/meetlite/config.json` by
default.

Create the file:

```bash
meetlite config init
```

Print the active path:

```bash
meetlite config path
```

Use `MEETLITE_CONFIG` or `meetlite --config PATH` to use a different file.

## Minimal config

Recording works without transcription or summary settings. The default config is
enough for the tested macOS and Linux recording paths:

```json
{
  "recording": {
    "sample_rate": 48000,
    "microphone_gain": 1.0,
    "system_gain": 0.8,
    "microphone_device": null,
    "system_device": null
  },
  "stt": null,
  "llm": null
}
```

`sample_rate` must be `48000`. Device names come from `meetlite devices`.

## Recording settings

| Field | Default | Description |
| --- | --- | --- |
| `sample_rate` | `48000` | Output WAV sample rate. |
| `microphone_gain` | `1.0` | Microphone gain before mixing. |
| `system_gain` | `0.8` | System-audio gain before mixing. |
| `microphone_device` | `null` | Use the default microphone unless set. |
| `system_device` | `null` | Linux ALSA fallback device when PulseAudio is unavailable. |

On macOS, system audio is captured from the default output through
`MeetliteCapture.app`; `recording.system_device` is not used.

On Linux, Meetlite first records the current default PulseAudio sink monitor.
PipeWire works when its PulseAudio compatibility server is running. Debian 11 is
just the distro this path has been tested on so far, not a special target. If no
PulseAudio monitor is available, set `recording.system_device` to an ALSA PCM
capture device. For `snd-aloop`, use the capture side paired with your playback
device, for example `hw:Loopback,1,0` when audio is sent to `hw:Loopback,0,0`.

## Transcription settings

`stt` is required only for `meetlite transcribe` or `meetlite record --transcribe`.
It must point to an OpenAI-compatible multipart transcription endpoint.

```json
{
  "stt": {
    "base_url": "https://stt.example.com/v1",
    "transcription_path": "/audio/transcriptions",
    "model": "whisper-large-v3",
    "language": "en",
    "response_format": "verbose_json",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_STT_API_KEY"
    }
  }
}
```

`base_url` must start with `http://` or `https://`. `transcription_path` defaults
to `/audio/transcriptions`, and `response_format` defaults to `verbose_json`.

## Summary settings

`llm` is required only for `meetlite summarize` or `meetlite record --summarize`.
It uses an OpenAI-compatible chat-completions endpoint.

```json
{
  "llm": {
    "base_url": "https://llm.example.com/v1",
    "chat_completions_path": "/chat/completions",
    "model": "gpt-4o-mini",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_LLM_API_KEY"
    },
    "instructions": "Correct Acme to Acme Corp and use the spelling Nia Chen."
  }
}
```

`chat_completions_path` defaults to `/chat/completions`. `instructions` is
optional text sent with every summary request for names, terminology, and other
corrections.

## Authentication

Do not place API keys directly in the configuration file. Reference environment
variables instead.

Bearer token:

```json
"auth": { "type": "bearer", "token_env": "MEETLITE_STT_API_KEY" }
```

Custom header:

```json
"auth": {
  "type": "header",
  "header_name": "X-API-Key",
  "value_env": "MEETLITE_STT_API_KEY"
}
```

No authentication:

```json
"auth": { "type": "none" }
```

## Local whisper.cpp

For a local `whisper-server`, configure its native endpoint:

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

The Nix development shell includes `whisper-server`; provide a compatible GGML
model when starting it:

```bash
whisper-server --model /path/to/ggml-base.en.bin --port 8080
```
