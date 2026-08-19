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
  }
}
```

`recording` is optional. The recorder currently requires `sample_rate` to be
`48000`. Device names come from `meetlite devices`.

`stt` is required only for transcription. `base_url` must start with `http://`
or `https://`; `transcription_path` defaults to `/audio/transcriptions`.

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
