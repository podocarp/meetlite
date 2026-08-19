# Meetlite

> [!NOTE]
> Meetlite is still heavily WIP and not ready for release yet. It currently only
> works on MacOS but other platforms are planned additions.

Meetlite is a simple CLI meeting recorder and optional transcriber and
summarizer. It's born out of my frustration at the lack of a simple,
low-resource consumption tool that runs on all devices. It is heavily inspired
by [meetily](https://github.com/Zackriya-Solutions/meetily) but I wanted to
solve two problems with such offerings:
- Bloat: I didnt' want another JS stack running a GUI eating into the precious
VRAM for me to run larger whisper and LLM models. I am already heavily swapping
on a macbook air with all the electron crap and browsers, I have no resources to
run another GUI and whisper and llama at the back.
- External models: meetily, due to wanting to handle transcription and
summarization locally (it does make the happy path faster for regular users),
had difficulty hooking into external APIs. There has been talk of providing a
meetily API. I'm sure it can be done at some point in time, but I didnt' really
like that. `Meetlite` does not force you into anything so you can choose to use
large hosted models, or self-hosted models, or run directly on-device, or even
just save the wav files and do whatever (fine tuning etc.).

In summary: unix philosophy. I just want a tool that does recording, mixing,
filtering really well (it is harder than you think!), and optionally hooks into
separate transcription and summarization APIs downstream for convenience.

## Install

1. Open the [latest release](https://github.com/podocarp/meetlite/releases/latest).
2. Download `meetlite-macos-aarch64.zip` for Apple Silicon or
   `meetlite-macos-x86_64.zip` for an Intel Mac.
3. Unzip it and run `meetlite setup` from the folder containing `meetlite`.
```bash
./meetlite setup
```
> [!NOTE]
> On MacOS this downloads an additional app from github releases (you can also do this
> manually, but why) to `~/Library/Application Support/Meetlite/MeetliteCapture.app`.
> This core recording app handles signing and TCC on MacOS so that the CLI is
> more ergonomic and you don't need to wrangle with `open` and flags. All this
> is handled transparently for you, so just treat this as a heads up. Other than
> this everything works the same across all platforms.

You can move `meetlite` anywhere you prefer and run it with `./meetlite`.

## Quick Start

List microphones:
```bash
./meetlite devices
```

Record until you press Ctrl-C:
```bash
./meetlite record
```

Record for one minute into a chosen directory:
```bash
./meetlite record --duration 60 --output ./team-sync
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

Or record and transcribe live:

```bash
./meetlite transcribe --duration 60 --output ./live-sync
```

Live transcription preserves `audio.wav`, writes progress to `transcript.jsonl`,
and writes the completed result to `transcript.json`.

## More references/docs

- [Configuration reference](docs/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Build from source](docs/building.md)
