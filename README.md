# Meetlite

Meetlite is a macOS command-line meeting recorder. It saves microphone and
system audio to a local WAV file, and can transcribe recordings with an
OpenAI-compatible speech-to-text service.

## Install

Meetlite currently builds from source. You need macOS 14.4 or later, full Xcode,
and [Nix with flakes enabled](https://nixos.org/download/).

```bash
git clone https://github.com/podocarp/meetlite.git
cd meetlite
nix develop --command cargo install --path . --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
meetlite setup
```

Add `$HOME/.local/bin` to your shell profile to keep `meetlite` on your `PATH`.

`meetlite setup` downloads the macOS system-audio companion and installs it in
your Application Support directory. The first recording may prompt for
Microphone and Audio Capture permission. Grant both. If permission was denied,
enable **Meetlite Capture** in **System Settings > Privacy & Security > Audio
Capture**.

## Quick Start

List microphones:

```bash
meetlite devices
```

Record until you press Ctrl-C:

```bash
meetlite record
```

Record for one minute into a chosen directory:

```bash
meetlite record --duration 60 --output ./team-sync
```

Meetlite writes `audio.wav` and `metadata.json` to the output directory. Without
`--output`, it creates a timestamped `meetlite-...` directory in the current
working directory.

## Transcription

Create the default configuration file:

```bash
meetlite config init
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
meetlite transcribe ./team-sync/audio.wav
```

Or record and transcribe live:

```bash
meetlite transcribe --duration 60 --output ./live-sync
```

Live transcription preserves `audio.wav`, writes progress to `transcript.jsonl`,
and writes the completed result to `transcript.json`.

## Reference

- [Configuration reference](docs/configuration.md)
- [Troubleshooting](docs/troubleshooting.md)
