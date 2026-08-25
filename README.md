<p align="center">
  <img src="assets/meetlite-banner.svg" alt="Meetlite banner" width="100%">
</p>

# Meetlite

> [!NOTE]
> Meetlite is early software. Release artifacts are experimental; back up
> recordings and verify capture on your hardware before relying on it.

Meetlite is a simple CLI meeting recorder and optional transcriber and
summarizer. It's born out of my frustration at the lack of a simple,
low-resource consumption tool that can eventually work across the machines I
actually use.

It is heavily inspired by [meetily](https://github.com/Zackriya-Solutions/meetily)
but I wanted to solve two problems with such offerings:

- Bloat: I didn't want another JS stack running a GUI eating into the precious
  VRAM I need for larger whisper and LLM models. I am already heavily swapping
  on a MacBook Air with all the Electron apps and browsers; I have no resources
  to run another GUI and whisper and llama in the background.
- External models: meetily, due to wanting to handle transcription and
  summarization locally, had difficulty hooking into external APIs. `Meetlite`
  does not force you into anything, so you can choose hosted models, self-hosted
  models, on-device models, or just save the WAV files and do whatever you want
  with them later.

In summary: unix philosophy. I just want a tool that does recording, mixing,
filtering really well (it is harder than you think!), and optionally hooks into
separate transcription and summarization APIs downstream for convenience.

## Platform status

The goal is cross-platform recording. The current checkpoint is:

- macOS recording has been tested on Apple Silicon.
- Linux recording has been tested on Debian 11 x86_64.

That does **not** mean Debian is the only Linux target. The Linux path records the
current default PulseAudio monitor, which is the common desktop route and also
works on PipeWire systems with PulseAudio compatibility enabled. If no PulseAudio
server is available, configure an ALSA fallback such as `snd-aloop`.

Windows system-audio capture is not implemented yet.

## Install

### macOS

1. Open the [latest release](https://github.com/podocarp/meetlite/releases/latest).
2. Download `meetlite-macos-aarch64.zip` for Apple Silicon.
3. Unzip it and run `meetlite setup` from the folder containing `meetlite`.

```bash
./meetlite setup
```

> [!NOTE]
> On macOS this downloads an additional app from GitHub Releases to
> `~/Library/Application Support/Meetlite/MeetliteCapture.app`. This capture
> companion handles signing and TCC permissions so the CLI can capture system
> audio without requiring manual `open` commands or flags.

You can move `meetlite` anywhere you prefer and run it with `./meetlite`.

### Linux

1. Open the [latest release](https://github.com/podocarp/meetlite/releases/latest).
2. Download `meetlite-linux-x86_64.tar.gz`.
3. Extract the CLI:

```bash
tar -xzf meetlite-linux-x86_64.tar.gz
./meetlite record
```

The Linux archive is currently not self-contained. It requires a compatible
glibc plus PulseAudio and ALSA runtime libraries on the host. On Debian/Ubuntu,
that usually means:

```bash
sudo apt install libasound2 libpulse0 ca-certificates
```

Meetlite captures the current default PulseAudio monitor automatically,
including PipeWire systems with PulseAudio compatibility enabled. If no
PulseAudio server is available, configure an ALSA fallback in
`recording.system_device`; `snd-aloop` uses values such as `hw:Loopback,1,0`.
See [Build from source](docs/building.md) for build prerequisites and headless
capture tests.

## Quick Start

Core commands:

```bash
meetlite devices # list microphones
meetlite record  # records only.
meetlite record --transcribe # records with live transcription.
meetlite record --summarize  # records, live-transcribes, then writes summary.md.
meetlite transcribe <AUDIO_FILE> # transcribe an existing recording
meetlite summarize <TRANSCRIPT>  # summarize an existing transcript
```

Of course you can pass `--help` to any of them to get a list of flags and
commands. For instance record for one minute into a chosen directory:

```bash
meetlite record --duration 60 --output ./team-sync
```

Meetlite writes `audio.wav` and `metadata.json` to the output directory. Without
`--output`, it creates a timestamped `meetlite-...` directory in the current
working directory.

## Transcription

Create the default configuration file:

```bash
./meetlite config init
```

Edit `~/.config/meetlite/config.json` to add your speech-to-text provider. For
example, use an API key kept in an environment variable:

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

Set the key, then transcribe an existing recording:

```bash
export MEETLITE_STT_API_KEY='...'
./meetlite transcribe ./team-sync/audio.wav
```

Record and transcribe live:

```bash
./meetlite record --transcribe --duration 60 --output ./live-sync
```

Live transcription preserves `audio.wav`, writes progress to `transcript.jsonl`,
and writes the completed result to `transcript.json`.

Meetlite refuses to replace an existing output directory or generated transcript
by default. Use `--force` to replace Meetlite artifacts in a specified recording
directory or an existing `transcript.json`.

## Summaries

Configure an OpenAI-compatible chat-completions endpoint in the `llm` section,
then summarize one existing transcript. Meetlite writes `summary.md` beside the
transcript; summaries are optional and never affect recording or transcription
artifacts.

For example:

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

`record --summarize` requires both the `stt` and `llm` sections. Set the LLM API
key before running either summary command:

```bash
export MEETLITE_LLM_API_KEY='...'
```

```bash
./meetlite summarize ./team-sync/transcript.json
```

Meetlite refuses to replace an existing `summary.md`; use `--force` to rewrite
it.

Use `llm.instructions` for names, terminology, and other transcription
corrections before the model creates the Markdown summary.

To run the full recording pipeline, including the final summary:

```bash
./meetlite record --summarize --duration 60 --output ./team-sync
```

## More references/docs

- [Configuration reference](docs/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Build from source](docs/building.md)
- [Project plan](PLAN.md)
