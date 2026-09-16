#!/usr/bin/env bash
set -euo pipefail

readonly repo_root="$(git rev-parse --show-toplevel)"
readonly identity="${MEETLITE_CODESIGN_IDENTITY:--}"
readonly output_dir="${MEETLITE_OUTPUT_DIR:-$repo_root/dist}"
readonly capture_app="$output_dir/MeetliteCapture.app"
readonly gui_app="$output_dir/Meetlite.app"
readonly cli="$output_dir/meetlite"
readonly package_version="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json, sys; metadata = json.load(sys.stdin); print(next(package["version"] for package in metadata["packages"] if package["name"] == "meetlite"))')"
release_version="${MEETLITE_RELEASE_VERSION:-$package_version}"
release_version="${release_version#v}"
readonly release_version
readonly bundle_version="${release_version%%[-+]*}"

if [[ "$release_version" != "$package_version" ]]; then
  printf 'Release version %s does not match Cargo package version %s\n' "$release_version" "$package_version" >&2
  exit 1
fi
if [[ ! "$bundle_version" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  printf 'Cargo package version %s cannot be used as a macOS bundle version\n' "$package_version" >&2
  exit 1
fi

cargo build --release --locked --bin meetlite
rm -rf "$capture_app" "$gui_app"
mkdir -p "$output_dir"
cp "$repo_root/target/release/meetlite" "$cli"

cargo build --release --locked --bin meetlite-gui
mkdir -p "$gui_app/Contents/MacOS" "$gui_app/Contents/Resources"
cp "$repo_root/Meetlite-Info.plist" "$gui_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $bundle_version" "$gui_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $bundle_version" "$gui_app/Contents/Info.plist"
cp "$repo_root/target/release/meetlite-gui" "$gui_app/Contents/MacOS/meetlite-gui"
cp "$cli" "$gui_app/Contents/Resources/meetlite"

mkdir -p "$capture_app/Contents/MacOS"
cp "$repo_root/MeetliteCapture-Info.plist" "$capture_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $bundle_version" "$capture_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $bundle_version" "$capture_app/Contents/Info.plist"
MEETLITE_EMBEDDED_INFO_PLIST="$capture_app/Contents/Info.plist" cargo build --release --locked --bin meetlite
cp "$repo_root/target/release/meetlite" "$capture_app/Contents/MacOS/meetlite"
codesign --force --sign "$identity" "$capture_app"
ditto "$capture_app" "$gui_app/Contents/Resources/MeetliteCapture.app"
codesign --force --sign "$identity" "$gui_app/Contents/Resources/MeetliteCapture.app"
codesign --force --sign "$identity" "$gui_app"
codesign --verify --deep --strict --verbose=2 "$capture_app"
codesign --verify --deep --strict --verbose=2 "$gui_app/Contents/Resources/MeetliteCapture.app"
codesign --verify --deep --strict --verbose=2 "$gui_app"
printf 'Built %s, %s, and %s\n' "$cli" "$capture_app" "$gui_app"
