#!/usr/bin/env bash
set -euo pipefail

# Build before scheduling playback so compilation time cannot consume the test.
# The delayed player gives Core Audio time to start and request permissions.
readonly repo_root="$(git rev-parse --show-toplevel)"
readonly cli="$repo_root/dist/meetlite"
readonly tone_frequency=1000
readonly tone_duration_seconds=10
readonly duration_seconds=14
readonly playback_delay_seconds=2
readonly playback_rate=1
readonly temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/meetlite-tone-test.XXXXXX")"
readonly fixture="$temporary_directory/tone.wav"
readonly output_directory="$temporary_directory/recording"

python3 - "$fixture" "$tone_frequency" "$tone_duration_seconds" <<'PY'
import math
import sys
import wave

path = sys.argv[1]
frequency = float(sys.argv[2])
duration = float(sys.argv[3])
rate = 48_000
amplitude = 0.35
fade_samples = int(rate * 0.02)
sample_count = int(rate * duration)
with wave.open(path, "wb") as recording:
    recording.setnchannels(1)
    recording.setsampwidth(2)
    recording.setframerate(rate)
    frames = bytearray()
    for index in range(sample_count):
        envelope = 1.0
        if index < fade_samples:
            envelope = index / fade_samples
        elif sample_count - index <= fade_samples:
            envelope = (sample_count - index) / fade_samples
        sample = round(math.sin(2 * math.pi * frequency * index / rate) * amplitude * envelope * 32767)
        frames.extend(int(sample).to_bytes(2, "little", signed=True))
    recording.writeframes(frames)
PY

silent_recording="$temporary_directory/silent.wav"
python3 - "$silent_recording" <<'PY'
import sys
import wave

with wave.open(sys.argv[1], "wb") as recording:
    recording.setnchannels(1)
    recording.setsampwidth(2)
    recording.setframerate(48_000)
    recording.writeframes(bytes(48_000 * 2))
PY
if python3 "$repo_root/scripts/analyze-recording.py" \
  "$fixture" \
  "$silent_recording" \
  "$playback_rate" >/dev/null 2>&1; then
  printf 'Silent recording was accepted by analyzer\n' >&2
  exit 1
fi

bash "$repo_root/scripts/build-macos-app.sh"

(
  sleep "$playback_delay_seconds"
  afplay -r "$playback_rate" "$fixture"
) &
player_pid=$!

"$cli" record \
  --duration "$duration_seconds" \
  --no-microphone \
  --output "$output_directory"

wait "$player_pid" || true
afinfo "$output_directory/audio.wav"
python3 "$repo_root/scripts/analyze-recording.py" \
  "$fixture" \
  "$output_directory/audio.wav" \
  "$playback_rate"
test -s "$output_directory/metadata.json"
printf 'Recording written to %s\n' "$output_directory/audio.wav"
