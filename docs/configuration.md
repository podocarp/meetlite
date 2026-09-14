# Configuration

Meetlite reads JSON configuration from `~/.config/meetlite/config.json` by
default. The configuration directory is created with `0700` permissions and the
configuration file is written with `0600` permissions on Unix platforms.

Create the file:

```bash
meetlite config init
```

Configure transcription or summaries:

```bash
meetlite config setup stt
meetlite config setup llm
```

Print the active path:

```bash
meetlite config path
```

Use `MEETLITE_CONFIG` or `meetlite --config PATH` to use a different file.

## Default config

Recording works out of the box. The default config also includes OpenAI-compatible
STT and LLM examples so setup is discoverable:

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
    "api_style": "openai-compatible",
    "base_url": "https://api.openai.com/v1",
    "transcription_path": "/audio/transcriptions",
    "model": "whisper-1",
    "language": null,
    "prompt": null,
    "response_format": "verbose_json",
    "auth": {
      "type": "bearer_plain",
      "token": ""
    }
  },
  "llm": {
    "api_style": "openai-compatible",
    "base_url": "https://api.openai.com/v1",
    "chat_completions_path": "/chat/completions",
    "model": "gpt-4o-mini",
    "auth": {
      "type": "bearer_plain",
      "token": ""
    },
    "instructions": null
  }
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

`stt` is required for `meetlite start`, `meetlite transcribe`, and live
transcription through `meetlite record`. It must use a supported API style.
Currently, Meetlite supports `openai-compatible` multipart transcription APIs.

```json
{
  "stt": {
    "api_style": "openai-compatible",
    "base_url": "https://stt.example.com/v1",
    "transcription_path": "/audio/transcriptions",
    "model": "whisper-large-v3",
    "language": "en",
    "prompt": "Meetlite, Acme Corp, PostgreSQL.",
    "response_format": "verbose_json",
    "auth": {
      "type": "bearer_keyring",
      "service": "Meetlite",
      "user": "Meetlite STT API Key"
    }
  }
}
```

`api_style` defaults to `openai-compatible`. `base_url` must start with `http://` or `https://`. `transcription_path` defaults to `/audio/transcriptions`, and `response_format` defaults to `verbose_json`. `prompt` is an optional Whisper-style transcription hint for terminology and style. During chunked transcription, Meetlite adds the recent completed transcript after this hint for continuity; providers may enforce their own prompt-token limit.

## Summary settings

`llm` is required for `meetlite start`, `meetlite summarize`, and
`meetlite record --summarize`. It uses a supported API style. Currently,
Meetlite supports streaming `openai-compatible` chat-completions APIs.

```json
{
  "llm": {
    "api_style": "openai-compatible",
    "base_url": "https://llm.example.com/v1",
    "chat_completions_path": "/chat/completions",
    "model": "gpt-4o-mini",
    "auth": {
      "type": "bearer_keyring",
      "service": "Meetlite",
      "user": "Meetlite LLM API Key"
    },
    "instructions": "Correct Acme to Acme Corp and use the spelling Nia Chen."
  }
}
```

`api_style` defaults to `openai-compatible`. `chat_completions_path` defaults to `/chat/completions`. `instructions` is optional text sent with every summary request for names, terminology, and other corrections.

## Authentication

`meetlite config setup stt` and `meetlite config setup llm` try to store API keys
in the OS keyring. If the keyring is unavailable, Meetlite stores the key in the
private configuration file. On macOS, choose Allow or Always Allow if Keychain
prompts for access. Meetlite resolves STT credentials before recording starts
and reuses the result for every live-transcription request. `MEETLITE_STT_API_KEY`
and `MEETLITE_LLM_API_KEY` override configured bearer tokens when set.

OS keyring bearer token:

```json
"auth": { "type": "bearer_keyring", "service": "Meetlite", "user": "Meetlite STT API Key" }
```

Plain config bearer token:

```json
"auth": { "type": "bearer_plain", "token": "sk-..." }
```

Environment bearer token:

```json
"auth": { "type": "bearer", "token_env": "MEETLITE_STT_API_KEY" }
```

Custom header from environment:

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
    "api_style": "openai-compatible",
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
