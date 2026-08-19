#!/usr/bin/env bash
set -euo pipefail

for command in cargo pactl pacat python3; do
  if ! command -v "$command" >/dev/null; then
    printf '%s\n' "Missing required command: $command" >&2
    exit 1
  fi
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="$(mktemp -d)"
output_dir="$work_dir/recording"
log_file="$work_dir/record.log"
sink="meetlite_test_$$"
previous_sink="$(pactl get-default-sink)"
module="$(pactl load-module module-null-sink "sink_name=$sink")"

cleanup() {
  pactl set-default-sink "$previous_sink" >/dev/null 2>&1 || true
  pactl unload-module "$module" >/dev/null 2>&1 || true
}
trap cleanup EXIT

pactl set-default-sink "$sink"
(cd "$repo_root" && cargo build)
(cd "$repo_root" && target/debug/meetlite record --no-microphone --duration 8 --output "$output_dir") >"$log_file" 2>&1 &
capture_pid=$!

for _ in {1..10}; do
  if grep -q 'Recording to' "$log_file"; then
    break
  fi
  sleep 1
done
if ! grep -q 'Recording to' "$log_file"; then
  cat "$log_file" >&2
  exit 1
fi

python3 -c 'import math, struct, sys; rate = 48000; sys.stdout.buffer.write(b"".join(struct.pack("<h", int(0.5 * 32767 * math.sin(2 * math.pi * 440 * i / rate))) for i in range(rate * 2)))' \
  | pacat --playback --device="$sink" --format=s16le --rate=48000 --channels=1
wait "$capture_pid"

python3 - "$output_dir/audio.wav" <<'PY'
import sys
import wave

with wave.open(sys.argv[1]) as recording:
    samples = recording.readframes(recording.getnframes())
    peak = max(
        abs(int.from_bytes(samples[index:index + 2], "little", signed=True))
        for index in range(0, len(samples), 2)
    )
    print(f"frames={recording.getnframes()} rate={recording.getframerate()} peak={peak}")
    if recording.getframerate() != 48000 or peak <= 1000:
        raise SystemExit("PulseAudio monitor recording did not contain the generated tone")
PY
