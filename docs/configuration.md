# Configuration

Meetlite reads JSON configuration from `~/.config/meetlite/config.json` by
default. Create it with:

```bash
meetlite config init
```

Use `MEETLITE_CONFIG` or `meetlite --config PATH` to use a different file.

## Schema

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
  },
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

`recording` is optional. The recorder currently requires `sample_rate` to be
`48000`. Device names come from `meetlite devices`.

On Linux, Meetlite first records the current default PulseAudio sink monitor,
with no extra configuration. This also works with PipeWire when its
PulseAudio-compatibility server is running. If no PulseAudio monitor is
available, configure `recording.system_device` as an ALSA PCM fallback. For an
ALSA loopback card, use the capture side paired with your playback device, for
example `hw:Loopback,1,0` when audio is sent to `hw:Loopback,0,0`.

`stt` is required only for transcription. `base_url` must start with `http://`
or `https://`; `transcription_path` defaults to `/audio/transcriptions`.

`llm` is required only for `meetlite summarize`. It uses an OpenAI-compatible
chat-completions endpoint; `chat_completions_path` defaults to
`/chat/completions`. `instructions` is optional text sent with every summary
request for corrections such as names and domain terminology.

## Authentication

Do not place API keys directly in the configuration file. Reference an
environment variable instead.

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
