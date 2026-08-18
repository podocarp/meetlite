#!/usr/bin/env bash
set -euo pipefail

readonly repo_root="$(git rev-parse --show-toplevel)"
readonly app="${MEETLITE_APP_OUTPUT:-$repo_root/dist/Meetlite.app}"
readonly identity="${MEETLITE_CODESIGN_IDENTITY:--}"
readonly capture_app="$(dirname "$app")/MeetliteCapture.app"

case "$app" in
  "$repo_root"/dist/*.app) ;;
  *)
    printf 'MEETLITE_APP_OUTPUT must be an .app under %s/dist\n' "$repo_root" >&2
    exit 1
    ;;
esac

# The outer CLI and nested capture app have distinct stable TCC identities, so
# each executable embeds the matching plist before its bundle is signed.
MEETLITE_EMBEDDED_INFO_PLIST="$repo_root/Info.plist" cargo build --release
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$capture_app/Contents/MacOS"
cp "$repo_root/Info.plist" "$app/Contents/Info.plist"
cp "$repo_root/target/release/meetlite" "$app/Contents/MacOS/meetlite"
cp "$repo_root/MeetliteCapture-Info.plist" "$capture_app/Contents/Info.plist"
MEETLITE_EMBEDDED_INFO_PLIST="$repo_root/MeetliteCapture-Info.plist" cargo build --release
cp "$repo_root/target/release/meetlite" "$capture_app/Contents/MacOS/meetlite"
# Sign the TCC-owning nested app before signing its enclosing bundle.
codesign --force --sign "$identity" "$capture_app"
codesign --force --sign "$identity" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
printf 'Built %s\n' "$app"
