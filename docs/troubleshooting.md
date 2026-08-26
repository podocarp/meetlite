# Troubleshooting

## macOS Audio Capture permission

System audio requires macOS 14.4 or later and Audio Capture permission for
**Meetlite Capture**.

Install Meetlite with `scripts/install.sh`, start a recording, and accept the prompt:

```bash
meetlite record --duration 10 --output ./permission-test
```

If the prompt was denied, enable it in **System Settings > Privacy & Security >
Audio Capture**. Microphone recording separately requires Microphone permission.

## macOS: no system audio

Confirm that Meetlite is recording the default system output and play a known
audio source during a short test:

```bash
meetlite record --no-microphone --duration 10 --output ./system-test
```

Listen to `./system-test/audio.wav`. Re-run the installer if the capture agent
is missing or outdated.

Current macOS release agents are ad-hoc signed and not notarized. macOS can show
a Gatekeeper warning and may ask again for Audio Capture permission after an
agent update.

## Linux: no system audio

Meetlite records the current default PulseAudio monitor by default. Check that a
PulseAudio-compatible server is running:

```bash
pactl info
```

Then play audio and record a short system-only sample:

```bash
meetlite record --no-microphone --duration 10 --output ./linux-system-test
```

If PulseAudio is unavailable, configure an ALSA capture device in
`recording.system_device`. For `snd-aloop`, a common capture device is
`hw:Loopback,1,0`.

## Linux: missing libraries

If the release binary fails to start, install the runtime libraries. On
Debian/Ubuntu:

```bash
sudo apt install libasound2 libpulse0 ca-certificates
```

For source builds, install development headers too:

```bash
sudo apt install build-essential pkg-config libasound2-dev libpulse-dev
```

## No microphone devices

Run:

```bash
meetlite devices
```

If no microphone appears, check OS privacy settings, the selected default input
device, and whether another application has exclusive access to the device.

## Transcription errors

Run `meetlite config path` to find the active configuration file. Confirm the STT
service is reachable, its endpoint is correct, and the environment variable
referenced by `auth` is set in the shell running Meetlite.

Network failures do not discard `audio.wav`. Retry with:

```bash
meetlite transcribe ./recording/audio.wav
```
