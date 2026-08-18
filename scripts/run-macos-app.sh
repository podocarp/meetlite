#!/usr/bin/env bash
set -euo pipefail

# TCC associates Audio Capture permission with a signed app bundle. Build and
# launch the same bundle format that is distributed to macOS users.
readonly repo_root="$(git rev-parse --show-toplevel)"
readonly app="$repo_root/dist/Meetlite.app"

bash "$repo_root/scripts/build-macos-app.sh"
open -W "$app" --args "$@"
