#!/usr/bin/env bash
set -euo pipefail

# Build before scheduling playback so compilation time cannot consume the test.
# The delayed player gives Core Audio time to start and request permissions.
readonly repo_root="$(git rev-parse --show-toplevel)"
readonly fixture="$repo_root/beep-02.wav"
readonly app="$repo_root/dist/Meetlite.app"
readonly duration_seconds=8
readonly playback_delay_seconds=2
readonly temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/meetlite-beep-test.XXXXXX")"
readonly output_directory="$temporary_directory/recording"

if [[ ! -f "$fixture" ]]; then
  printf 'Missing test fixture: %s\n' "$fixture" >&2
  exit 1
fi

nix develop --command bash "$repo_root/scripts/build-macos-app.sh"

(
  sleep "$playback_delay_seconds"
  afplay -r 12 "$fixture"
) &
player_pid=$!

"$app/Contents/MacOS/meetlite" record \
  --duration "$duration_seconds" \
  --no-microphone \
  --output "$output_directory"

wait "$player_pid" || true
afinfo "$output_directory/audio.wav"
test -s "$output_directory/metadata.json"
printf 'Recording written to %s\n' "$output_directory/audio.wav"
