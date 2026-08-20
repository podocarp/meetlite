#!/usr/bin/env bash
set -euo pipefail

readonly repo_root="$(git rev-parse --show-toplevel)"
readonly identity="${MEETLITE_CODESIGN_IDENTITY:--}"
readonly output_dir="${MEETLITE_OUTPUT_DIR:-$repo_root/dist}"
readonly capture_app="$output_dir/MeetliteCapture.app"
readonly cli="$output_dir/meetlite"
readonly legacy_app="$output_dir/Meetlite.app"

# The raw CLI does not request TCC-protected system audio. Only the capture app
# embeds a plist and receives an app-bundle signature.
cargo build --release
rm -rf "$capture_app" "$legacy_app"
mkdir -p "$output_dir" "$capture_app/Contents/MacOS"
cp "$repo_root/target/release/meetlite" "$cli"
cp "$repo_root/MeetliteCapture-Info.plist" "$capture_app/Contents/Info.plist"
MEETLITE_EMBEDDED_INFO_PLIST="$repo_root/MeetliteCapture-Info.plist" cargo build --release
cp "$repo_root/target/release/meetlite" "$capture_app/Contents/MacOS/meetlite"
codesign --force --sign "$identity" "$capture_app"
codesign --verify --deep --strict --verbose=2 "$capture_app"
printf 'Built %s and %s\n' "$cli" "$capture_app"
