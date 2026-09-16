<p align="center">
  <img src="assets/meetlite-banner.svg" alt="Meetlite banner" width="100%">
</p>

# Meetlite

> [!NOTE]
> Meetlite is early software. Notably, windows support is still missing!

Meetlite is a simple meeting recorder with a CLI and lightweight native GUI,
plus optional transcription and summarization. It's born out of my frustration
at the lack of a simple,
low-resource consumption tool that can eventually work across the machines I
actually use.

In summary: Unix philosophy. I just want a tool that does recording really well
(it is harder than you think!), keeps sources separate for downstream tools, and
optionally hooks into transcription and summarization APIs for convenience.

It is heavily inspired by [meetily](https://github.com/Zackriya-Solutions/meetily)
but I wanted to solve two problems with such offerings:

- Bloat: I didn't want another JS stack running a GUI eating into the precious
  VRAM I need for larger whisper and LLM models. How to be local first if
  regular Joes can't run it locally?
- External models: `Meetlite` does not force you using any specific architecture
  or backends, so if you can't run models locally you can choose model
  providers, self-hosted models,  or just treat it as a regular voice recorder!

However, this will necessitate more setup, so if you're just looking for a
wheels included experience you can try out meetily instead.

## Platform status

The goal is cross-platform recording. Audio is actually quite hard since every
platform uses their own APIs (and Linux on its own has like 20). I've tested:
- macOS recording has been tested on Apple Silicon (Intel not supported).
- Linux (ALSA and PulseAudio) recording has been tested on Debian 11 x86_64.
- Linux (ALSA) recording has been tested on Debian 10 x86_64. Couldn't get PulseAudio to work.

Windows system-audio capture is not implemented yet.

## Install

```bash
curl -fsSL https://github.com/podocarp/meetlite/releases/latest/download/install.sh | sh
```

This installs the CLI and native GUI by default. The CLI is written to
`~/.local/bin` and that directory is added to your active shell profile when
needed. On Linux, `meetlite-gui` is installed beside it. On macOS,
`Meetlite.app` is installed to `~/Applications` and the CLI remains available in
`~/.local/bin`. Set `INSTALL_DIR` to choose another binary directory:

```bash
curl -fsSL https://github.com/podocarp/meetlite/releases/latest/download/install.sh | INSTALL_DIR=/usr/local/bin sh
```

> [!NOTE]
> On macOS the installer also installs the LaunchServices capture companion to
> `~/Library/Application Support/Meetlite/MeetliteCapture.app`. This companion
> handles signing and TCC permissions so the CLI can capture system audio without
> requiring manual `open` commands or flags. Set `APP_INSTALL_DIR` to install
> `Meetlite.app` somewhere other than `~/Applications`.

Use `--cli-only` to skip the GUI while retaining the CLI and required macOS
capture companion:

```bash
curl -fsSL https://github.com/podocarp/meetlite/releases/latest/download/install.sh | sh -s -- --cli-only
```

The same artifacts remain available for manual installation:

- macOS Apple Silicon: `Meetlite-macos-aarch64.app.zip`.
- Linux x86_64: `meetlite-gui-linux-x86_64.tar.gz`; keep `meetlite` and
  `meetlite-gui` together.

Neither Linux tarball is self-contained. Both require compatible glibc,
PulseAudio or ALSA, and CA certificates; the GUI tarball additionally requires
a graphical X11 or Wayland session, `libxkbcommon`, and OpenGL/EGL libraries.
On Debian/Ubuntu, typical runtime packages are:

```bash
sudo apt install libasound2 libpulse0 ca-certificates libxkbcommon0 libxkbcommon-x11-0 libgl1 libegl1 libwayland-client0 libwayland-cursor0 libwayland-egl1 libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrandr2
```

Meetlite captures the current default PulseAudio monitor automatically,
including PipeWire systems with PulseAudio compatibility enabled. If no
PulseAudio server is available, configure an ALSA fallback in
`recording.system_device`; `snd-aloop` uses values such as `hw:Loopback,1,0`.
See [Build from source](docs/building.md) for build prerequisites and headless
capture tests.

JACK is not supported.

Windows is not implemented, but will be once I get a windows machine to test on.

## Quick Start

Open `Meetlite.app` or run `./meetlite-gui` from the extracted Linux directory
to use the native GUI. It configures providers, starts the CLI recording
pipeline, displays live transcript and
summary output, and provides phase-aware stop controls. Recordings and generated
artifacts are preserved when the GUI session is reset.

Core CLI commands:

```bash
meetlite devices # list microphones
meetlite start   # records, live-transcribes, then streams and saves a summary
meetlite record  # records only
meetlite record --transcribe # records with live transcription
meetlite record --summarize  # alternate entry point for the start pipeline
meetlite transcribe <RECORDING_DIRECTORY> # transcribe an existing recording
meetlite summarize <TRANSCRIPT>  # stream and save a summary
```

Of course you can pass `--help` to any of them to get a list of flags and
commands. For instance record for one minute into a chosen directory:

```bash
meetlite record --duration 60 --output ./team-sync
```

Meetlite writes `microphone.wav` and/or `system.wav` as aligned mono 48 kHz
tracks, plus `metadata.json`, to the output directory. It never creates
`audio.wav`; mix both tracks externally when desired:

```bash
ffmpeg -i microphone.wav -i system.wav -filter_complex "amix=inputs=2:duration=longest:normalize=0" -ac 1 audio.wav
```

Without `--output`, Meetlite creates a timestamped `meetlite-...` directory in
the current working directory.

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
    "prompt": "Meetlite, Acme Corp, PostgreSQL.",
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
./meetlite transcribe ./team-sync
```

Record and transcribe live:

```bash
./meetlite record --transcribe --duration 60 --output ./live-sync
```

Live transcription streams completed 15-second transcript chunks to the terminal,
preserves the aligned source tracks, writes progress to `transcript.jsonl`, and
writes the completed result to `transcript.json`. The completed transcript
transcribes the separate source tracks and labels microphone speech as `You` and
system audio as `Remote`; individual remote participants remain unlabeled.

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

Summary Markdown is streamed to the terminal while it is generated and saved as
`summary.md`. Meetlite refuses to replace an existing summary; use `--force` to
rewrite it.

Use `llm.instructions` for names, terminology, and other transcription
corrections before the model creates the Markdown summary.

To run the full recording pipeline, including live transcription and the final
streamed summary:

```bash
./meetlite start --duration 60 --output ./team-sync
```

Use global `--json` for newline-delimited JSON progress events.

## More references/docs

- [Configuration reference](docs/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Build from source](docs/building.md)
- [GUI architecture](docs/gui.md)
