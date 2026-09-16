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

Print the active path or inspect redacted configuration usability:

```bash
meetlite config path
meetlite config status
```

The native GUI uses `meetlite --json config apply --stdin-json` to update settings. Its purpose-built JSON payload is sent through standard input so API keys never appear in process arguments. A managed saved key appears only as the literal non-secret mask `********`: leave it unchanged to preserve the Keychain value, clear it to remove that provider's managed key, or replace it with a new key.

Use `MEETLITE_CONFIG` or `meetlite --config PATH` to use a different file.

## Default config

Recording works out of the box. The default config also includes OpenAI-compatible
STT and LLM examples so setup is discoverable:

```json
{
  "recording": {
    "sample_rate": 48000,
    "microphone_device": null,
    "system_device": null
  },
  "summary_enabled": true,
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
| `sample_rate` | `48000` | Source WAV sample rate. |
| `microphone_device` | `null` | Use the default microphone unless set. |
| `system_device` | `null` | Linux ALSA fallback device when PulseAudio is unavailable. |

Each enabled source is saved as a timestamp-aligned mono 48 kHz WAV file:
`microphone.wav` for local speech and `system.wav` for meeting audio. Meetlite
never creates `audio.wav`; users may mix the tracks externally. Passing the
recording directory to `meetlite transcribe` transcribes both available tracks
and labels segments as `You` and `Remote` respectively. This does not
distinguish individual remote participants.

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
      "user": "Meetlite API Credentials"
    }
  }
}
```

`api_style` defaults to `openai-compatible`. `base_url` must start with `http://` or `https://`. `transcription_path` defaults to `/audio/transcriptions`, and `response_format` defaults to `verbose_json`. `prompt` is an optional Whisper-style transcription hint for terminology and style. During chunked transcription, Meetlite adds the recent completed transcript after this hint for continuity; providers may enforce their own prompt-token limit.

## Summary settings

`summary_enabled` controls whether `meetlite start` summarizes after transcription and defaults to `true`. `llm` is required when summaries are enabled, for `meetlite summarize`, and for `meetlite record --summarize`. It uses a supported API style. Currently, Meetlite supports streaming `openai-compatible` chat-completions APIs.

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
      "user": "Meetlite API Credentials"
    },
    "instructions": "Correct Acme to Acme Corp and use the spelling Nia Chen."
  }
}
```

`api_style` defaults to `openai-compatible`. `chat_completions_path` defaults to `/chat/completions`. `instructions` is optional text sent with every summary request for names, terminology, and other corrections.

## Authentication

`meetlite config setup stt` and `meetlite config setup llm` securely prompt for their separate API keys and store them together in one Meetlite OS-keyring item. This lets macOS authorize the item once while Meetlite selects the STT or LLM credential as needed. Setup fails without writing the key to the configuration file when the keyring is unavailable. On macOS, choose Allow or Always Allow if Keychain prompts for access. Meetlite resolves STT credentials before recording starts and reuses the result for every live-transcription request. `MEETLITE_STT_API_KEY` and `MEETLITE_LLM_API_KEY` override configured bearer tokens when set.

Older releases stored credentials in separate `Meetlite STT API Key` and
`Meetlite LLM API Key` items (and some used `meetlite/stt-api-key` or
`meetlite/llm-api-key`). Configurations that still reference those entries remain
read-compatible. To move a provider to the combined item, enter a replacement
key in Settings or run `meetlite config setup stt` or
`meetlite config setup llm`. The old item is left in place and may be removed
manually after the replacement configuration works.

OS keyring bearer token:

```json
"auth": { "type": "bearer_keyring", "service": "Meetlite", "user": "Meetlite API Credentials" }
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
