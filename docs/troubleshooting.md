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

Listen to `./system-test/system.wav`. Re-run the installer if the capture agent
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

If the CLI release binary fails to start, install its runtime libraries. On
Debian/Ubuntu:

```bash
sudo apt install libasound2 libpulse0 ca-certificates
```

The GUI archive is not self-contained and must keep `meetlite-gui` beside
`meetlite`. Run `./meetlite-gui` from the extracted directory. It also needs a
graphical X11 or Wayland session and native window, keyboard, and OpenGL/EGL
libraries:

```bash
sudo apt install libasound2 libpulse0 ca-certificates libxkbcommon0 libxkbcommon-x11-0 libgl1 libegl1 libwayland-client0 libwayland-cursor0 libwayland-egl1 libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrandr2
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

Network failures do not discard `microphone.wav` or `system.wav`. Retry paired
transcription with the recording directory:

```bash
meetlite transcribe ./recording
```
