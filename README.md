<p align="center">
  <img src="assets/meetlite-banner.svg" alt="Meetlite banner" width="100%">
</p>

# Meetlite

Meetlite is a small Rust CLI for recording meetings to local WAV files, with
optional OpenAI-compatible transcription and summarization.

It is built for people who want the Unix-style version of a meeting recorder:
record clean audio, keep the files locally, and choose your own transcription or
LLM provider.

> [!NOTE]
> Meetlite is early software. Release artifacts are experimental; back up
> recordings and verify capture on your hardware before relying on it.

## Supported platforms

Recording has been tested on:

- macOS 14.4+ on Apple Silicon, using the `MeetliteCapture.app` companion for
  system audio permissions.
- Debian 11 x86_64, using the default PulseAudio monitor for system audio.

Linux also works on PipeWire desktops when PulseAudio compatibility is enabled.
If PulseAudio is not available, configure an ALSA capture device such as
`snd-aloop`.

Windows system-audio capture is not supported yet.

## Install

### macOS

1. Open the [latest release](https://github.com/podocarp/meetlite/releases/latest).
2. Download `meetlite-macos-aarch64.zip`.
3. Unzip it and run setup once:

```bash
./meetlite setup
```

`meetlite setup` installs `MeetliteCapture.app` to
`~/Library/Application Support/Meetlite/MeetliteCapture.app`. macOS grants Audio
Capture permission to that companion app, not to the terminal binary.

Then start a recording:

```bash
./meetlite record
```

### Debian 11 / Linux x86_64

1. Open the [latest release](https://github.com/podocarp/meetlite/releases/latest).
2. Download `meetlite-linux-x86_64.tar.gz`.
3. Install runtime audio libraries and extract the CLI:

```bash
sudo apt install libasound2 libpulse0 ca-certificates
tar -xzf meetlite-linux-x86_64.tar.gz
./meetlite record
```

On normal Debian desktop sessions, Meetlite captures your microphone and the
current default PulseAudio monitor automatically.

## Quick start

```bash
meetlite devices                         # list microphones
meetlite record                          # record mic + system audio
meetlite record --duration 60            # stop after 60 seconds
meetlite record --output ./team-sync     # choose output directory
meetlite record --no-microphone          # system audio only
meetlite record --no-system-audio        # microphone only
```

Meetlite writes:

```text
team-sync/
  audio.wav
  metadata.json
```

Without `--output`, it creates a timestamped `meetlite-...` directory in the
current working directory. Press Ctrl-C to stop a recording cleanly.

## Transcription

Create a config file:

```bash
meetlite config init
```

Edit `~/.config/meetlite/config.json` and add an OpenAI-compatible speech-to-text
provider. Keep API keys in environment variables:

```json
{
  "stt": {
    "base_url": "https://stt.example.com/v1",
    "model": "whisper-large-v3",
    "language": "en",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_STT_API_KEY"
    }
  }
}
```

Transcribe an existing recording:

```bash
export MEETLITE_STT_API_KEY='...'
meetlite transcribe ./team-sync/audio.wav
```

Record and transcribe live:

```bash
meetlite record --transcribe --duration 60 --output ./live-sync
```

Live transcription keeps `audio.wav`, writes progress to `transcript.jsonl`, and
writes the final transcript to `transcript.json`.

## Summaries

Configure an OpenAI-compatible chat-completions endpoint in the `llm` section:

```json
{
  "llm": {
    "base_url": "http://127.0.0.1:8321/v1",
    "model": "gpt-4o-mini",
    "auth": {
      "type": "bearer",
      "token_env": "MEETLITE_LLM_API_KEY"
    },
    "instructions": "Correct names and domain terminology before summarizing."
  }
}
```

Then summarize a transcript:

```bash
export MEETLITE_LLM_API_KEY='...'
meetlite summarize ./team-sync/transcript.json
```

Or run the full pipeline:

```bash
meetlite record --summarize --duration 60 --output ./team-sync
```

Meetlite writes `summary.md` beside the transcript. It will not overwrite an
existing transcript or summary unless you pass `--force`.

## More docs

- [Configuration reference](docs/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Build from source](docs/building.md)
- [Project plan](PLAN.md)
