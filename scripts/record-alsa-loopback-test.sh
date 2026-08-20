#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  printf '%s\n' 'This test requires Linux.' >&2
  exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="$(mktemp -d)"
output_dir="$work_dir/recording"
config="$(mktemp)"
device="${MEETLITE_LINUX_SYSTEM_DEVICE:-hw:Loopback,1,0}"
log_file="$work_dir/record.log"

cleanup() {
  rm -f "$config"
}
trap cleanup EXIT

printf '{"recording":{"system_device":"%s"}}\n' "$device" > "$config"

(cd "$repo_root" && cargo build)
(cd "$repo_root" && target/debug/meetlite --config "$config" record --no-microphone --duration 8 --output "$output_dir") >"$log_file" 2>&1 &
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
  | aplay -D hw:Loopback,0,0 -f S16_LE -r 48000 -c 1
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
        raise SystemExit("loopback recording did not contain the generated tone")
PY
