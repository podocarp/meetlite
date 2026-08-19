# Troubleshooting

## Audio Capture permission

System audio requires macOS 14.4 or later and Audio Capture permission for
**Meetlite Capture**. Run `meetlite setup`, start a recording, and accept the
prompt. If the prompt was denied, enable it in **System Settings > Privacy &
Security > Audio Capture**.

Microphone recording separately requires Microphone permission.

## No system audio

Confirm that Meetlite is recording the default system output and play a known
audio source during a short test:

```bash
meetlite record --no-microphone --duration 10 --output ./system-test
```

Listen to `./system-test/audio.wav`. Run `meetlite setup` again if the capture
agent is missing or outdated.

## Transcription errors

Run `meetlite config path` to find the active configuration file. Confirm the
STT service is reachable, its endpoint is correct, and the environment variable
referenced by `auth` is set in the shell running Meetlite.

Network failures do not discard `audio.wav`. Retry with:

```bash
meetlite transcribe ./recording/audio.wav
```

## Ad-hoc macOS releases

Current release agents are ad-hoc signed and not notarized. macOS can show a
Gatekeeper warning and may ask again for Audio Capture permission after an
agent update.
