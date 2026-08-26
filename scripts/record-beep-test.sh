#!/usr/bin/env bash
set -euo pipefail

# Build before scheduling playback so compilation time cannot consume the test.
# The delayed player gives Core Audio time to start and request permissions.
readonly repo_root="$(git rev-parse --show-toplevel)"
readonly fixture="$repo_root/beep-02.wav"
readonly cli="$repo_root/dist/meetlite"
readonly duration_seconds=8
readonly playback_delay_seconds=2
readonly playback_rate=12
readonly temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/meetlite-beep-test.XXXXXX")"
readonly output_directory="$temporary_directory/recording"

if [[ ! -f "$fixture" ]]; then
  printf 'Missing test fixture: %s\n' "$fixture" >&2
  exit 1
fi

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
