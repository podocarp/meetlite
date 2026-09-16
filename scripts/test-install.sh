#!/usr/bin/env bash
set -euo pipefail

readonly repo_root="$(git rev-parse --show-toplevel)"
readonly fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/meetlite-install-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

make_fake_bin() {
  local directory="$1"
  local os="$2"
  local arch="$3"
  mkdir -p "$directory"
  cat > "$directory/uname" <<EOF
#!/bin/sh
if [ "\${1:-}" = "-s" ]; then printf '%s\n' '$os'; else printf '%s\n' '$arch'; fi
EOF
  chmod +x "$directory/uname"
  cat > "$directory/curl" <<'EOF'
#!/bin/sh
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
cp "$MEETLITE_TEST_ASSETS/${url##*/}" "$output"
EOF
  chmod +x "$directory/curl"
  cat > "$directory/ditto" <<'EOF'
#!/bin/sh
cp -R "$1" "$2"
EOF
  chmod +x "$directory/ditto"
  cat > "$directory/codesign" <<'EOF'
#!/bin/sh
exit 0
EOF
  chmod +x "$directory/codesign"
}

make_linux_assets() {
  local assets="$1"
  local package="$fixture_root/linux-package"
  mkdir -p "$assets" "$package"
  printf '#!/bin/sh\n' > "$package/meetlite"
  printf '#!/bin/sh\n' > "$package/meetlite-gui"
  chmod +x "$package/meetlite" "$package/meetlite-gui"
  tar -C "$package" -czf "$assets/meetlite-gui-linux-x86_64.tar.gz" meetlite meetlite-gui
  tar -C "$package" -czf "$assets/meetlite-linux-x86_64.tar.gz" meetlite
}

make_macos_assets() {
  local assets="$1"
  local package="$fixture_root/macos-package"
  local app="$package/Meetlite.app"
  local capture="$app/Contents/Resources/MeetliteCapture.app"
  local cli_package="$package/meetlite-macos-aarch64"
  mkdir -p "$assets" "$app/Contents/MacOS" "$capture/Contents/MacOS" "$cli_package/MeetliteCapture.app/Contents/MacOS"
  printf '#!/bin/sh\n' > "$app/Contents/MacOS/meetlite-gui"
  printf '#!/bin/sh\n' > "$app/Contents/Resources/meetlite"
  printf '#!/bin/sh\n' > "$capture/Contents/MacOS/meetlite"
  printf '#!/bin/sh\n' > "$cli_package/meetlite"
  cp "$capture/Contents/MacOS/meetlite" "$cli_package/MeetliteCapture.app/Contents/MacOS/meetlite"
  chmod +x "$app/Contents/MacOS/meetlite-gui" "$app/Contents/Resources/meetlite" "$capture/Contents/MacOS/meetlite" "$cli_package/meetlite" "$cli_package/MeetliteCapture.app/Contents/MacOS/meetlite"
  (cd "$package" && zip -qr "$assets/Meetlite-macos-aarch64.app.zip" Meetlite.app)
  (cd "$package" && zip -qr "$assets/meetlite-macos-aarch64.zip" meetlite-macos-aarch64)
}

run_linux_case() {
  local name="$1"
  shift
  local root="$fixture_root/$name"
  local assets="$root/assets"
  local fake_bin="$root/bin"
  local home="$root/home"
  local install_dir="$root/install"
  make_fake_bin "$fake_bin" Linux x86_64
  make_linux_assets "$assets"
  mkdir -p "$home"
  PATH="$fake_bin:/usr/bin:/bin" \
    HOME="$home" \
    SHELL=/bin/sh \
    INSTALL_DIR="$install_dir" \
    MEETLITE_TEST_ASSETS="$assets" \
    sh "$repo_root/scripts/install.sh" "$@"
  test -x "$install_dir/meetlite"
  if [ "$name" = default ]; then
    test -x "$install_dir/meetlite-gui"
  else
    test ! -e "$install_dir/meetlite-gui"
  fi
}

run_macos_case() {
  local name="$1"
  shift
  local root="$fixture_root/macos-$name"
  local assets="$root/assets"
  local fake_bin="$root/bin"
  local home="$root/home"
  local install_dir="$root/install"
  local app_dir="$root/apps"
  make_fake_bin "$fake_bin" Darwin arm64
  make_macos_assets "$assets"
  mkdir -p "$home"
  PATH="$fake_bin:/usr/bin:/bin" \
    HOME="$home" \
    SHELL=/bin/sh \
    INSTALL_DIR="$install_dir" \
    APP_INSTALL_DIR="$app_dir" \
    MEETLITE_TEST_ASSETS="$assets" \
    sh "$repo_root/scripts/install.sh" "$@"
  test -x "$install_dir/meetlite"
  test -x "$home/Library/Application Support/Meetlite/MeetliteCapture.app/Contents/MacOS/meetlite"
  if [ "$name" = default ]; then
    test -x "$app_dir/Meetlite.app/Contents/MacOS/meetlite-gui"
  else
    test ! -e "$app_dir/Meetlite.app"
  fi
}

run_linux_case default
run_linux_case cli-only --cli-only
run_macos_case default
run_macos_case cli-only --cli-only

echo "installer tests passed"
