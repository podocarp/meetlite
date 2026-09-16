#!/bin/sh
set -eu

repo="${MEETLITE_REPO:-podocarp/meetlite}"
version="${MEETLITE_VERSION:-latest}"
install_dir="${INSTALL_DIR:-$HOME/.local/bin}"
app_install_dir="${APP_INSTALL_DIR:-$HOME/Applications}"
cli_only=0
tmpdir=""

say() {
  printf '%s\n' "$*" >&2
}

fail() {
  say "meetlite installer: $*"
  exit 1
}

usage() {
  cat >&2 <<'EOF'
Usage: install.sh [--cli-only]

By default, install the Meetlite CLI and native GUI.
  --cli-only  Install only the CLI and required macOS capture companion.
  -h, --help  Show this help.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --cli-only)
      cli_only=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      fail "unknown option: $1"
      ;;
  esac
  shift
done

need() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

download() {
  url="$1"
  output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$output" "$url"
  else
    fail "missing required command: curl or wget"
  fi
}

cleanup() {
  if [ -n "$tmpdir" ] && [ -d "$tmpdir" ]; then
    rm -rf "$tmpdir"
  fi
}

profile_file() {
  shell_name="$(basename "${SHELL:-sh}")"
  case "$shell_name" in
    zsh)
      printf '%s\n' "$HOME/.zshrc"
      ;;
    bash)
      printf '%s\n' "$HOME/.bashrc"
      ;;
    fish)
      printf '%s\n' "$HOME/.config/fish/config.fish"
      ;;
    *)
      printf '%s\n' "$HOME/.profile"
      ;;
  esac
}

add_to_path() {
  case ":$PATH:" in
    *":$install_dir:"*)
      return 0
      ;;
  esac

  profile="$(profile_file)"
  mkdir -p "$(dirname "$profile")"
  touch "$profile"

  if grep -F "$install_dir" "$profile" >/dev/null 2>&1; then
    say "$install_dir is already mentioned in $profile"
    return 0
  fi

  shell_name="$(basename "${SHELL:-sh}")"
  marker="# Added by meetlite installer"
  case "$shell_name" in
    fish)
      printf '\n%s\nfish_add_path %s\n' "$marker" "$install_dir" >> "$profile"
      ;;
    *)
      printf '\n%s\nexport PATH="%s:$PATH"\n' "$marker" "$install_dir" >> "$profile"
      ;;
  esac
  say "Added $install_dir to PATH in $profile"
  say "Restart your shell or run: export PATH=\"$install_dir:\$PATH\""
}

install_app_bundle() {
  app="$1"
  destination="$2"
  label="$3"
  executable="$4"
  [ -d "$app" ] || fail "release archive did not contain $label.app"
  [ -f "$app/Contents/MacOS/$executable" ] || fail "$label.app does not contain Contents/MacOS/$executable"

  parent="$(dirname "$destination")"
  staged="$parent/$label-install-$$.app"
  previous="$destination.previous"
  mkdir -p "$parent"
  rm -rf "$staged"
  ditto "$app" "$staged"
  codesign --verify --deep --strict "$staged" || fail "macOS rejected the $label code signature"
  rm -rf "$previous"
  had_current=0
  if [ -e "$destination" ]; then
    mv "$destination" "$previous"
    had_current=1
  fi
  if ! mv "$staged" "$destination"; then
    if [ "$had_current" = 1 ]; then
      mv "$previous" "$destination" || true
    fi
    fail "could not install $destination"
  fi
  say "Installed $label at $destination"
}

install_capture_app() {
  app="$1"
  destination="$HOME/Library/Application Support/Meetlite/MeetliteCapture.app"
  install_app_bundle "$app" "$destination" "MeetliteCapture" "meetlite"
  say "TCC: grant Meetlite Capture in System Settings > Privacy & Security > Audio Capture when macOS prompts."
}

install_gui_app() {
  app="$1"
  install_app_bundle "$app" "$app_install_dir/Meetlite.app" "Meetlite" "meetlite-gui"
}

trap cleanup EXIT INT TERM

os="$(uname -s)"
arch="$(uname -m)"
install_capture=0
install_gui=0

case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64)
    format="zip"
    install_capture=1
    if [ "$cli_only" = 1 ]; then
      asset="meetlite-macos-aarch64.zip"
    else
      asset="Meetlite-macos-aarch64.app.zip"
      install_gui=1
    fi
    ;;
  Linux:x86_64|Linux:amd64)
    format="tar.gz"
    if [ "$cli_only" = 1 ]; then
      asset="meetlite-linux-x86_64.tar.gz"
    else
      asset="meetlite-gui-linux-x86_64.tar.gz"
      install_gui=1
    fi
    ;;
  Darwin:*)
    fail "unsupported macOS architecture: $arch"
    ;;
  Linux:*)
    fail "unsupported Linux architecture: $arch"
    ;;
  *)
    fail "unsupported platform: $os $arch"
    ;;
esac

if [ "$version" = "latest" ]; then
  url="https://github.com/$repo/releases/latest/download/$asset"
else
  url="https://github.com/$repo/releases/download/$version/$asset"
fi

need uname
need mktemp
need mkdir
need install
need dirname
need basename
need grep

if [ "$install_capture" = 1 ]; then
  need ditto
  need codesign
fi

if [ "$format" = "zip" ]; then
  need unzip
else
  need tar
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/meetlite-install.XXXXXX")"
archive="$tmpdir/$asset"

say "Downloading $url"
download "$url" "$archive"

case "$format" in
  zip)
    unzip -q "$archive" -d "$tmpdir"
    ;;
  tar.gz)
    tar -xzf "$archive" -C "$tmpdir"
    ;;
esac

package_dir="$tmpdir"
case "$asset" in
  *.zip)
    package_name="${asset%.zip}"
    ;;
  *.tar.gz)
    package_name="${asset%.tar.gz}"
    ;;
esac
if [ -d "$tmpdir/$package_name" ]; then
  package_dir="$tmpdir/$package_name"
fi

cli_source="$package_dir/meetlite"
if [ "$os" = "Darwin" ] && [ "$install_gui" = 1 ]; then
  [ -d "$tmpdir/Meetlite.app" ] || fail "release archive did not contain Meetlite.app"
  cli_source="$tmpdir/Meetlite.app/Contents/Resources/meetlite"
fi
[ -f "$cli_source" ] || fail "release archive did not contain meetlite"

mkdir -p "$install_dir"
install -m 0755 "$cli_source" "$install_dir/meetlite"
say "Installed meetlite to $install_dir/meetlite"

if [ "$os" = "Darwin" ]; then
  if [ "$install_gui" = 1 ]; then
    install_gui_app "$tmpdir/Meetlite.app"
    install_capture_app "$tmpdir/Meetlite.app/Contents/Resources/MeetliteCapture.app"
  elif [ "$install_capture" = 1 ]; then
    install_capture_app "$package_dir/MeetliteCapture.app"
  fi
elif [ "$install_gui" = 1 ]; then
  [ -f "$package_dir/meetlite-gui" ] || fail "release archive did not contain meetlite-gui"
  install -m 0755 "$package_dir/meetlite-gui" "$install_dir/meetlite-gui"
  say "Installed meetlite-gui to $install_dir/meetlite-gui"
fi

add_to_path

if [ "$install_gui" = 1 ]; then
  if [ "$os" = "Darwin" ]; then
    say "Run: open $app_install_dir/Meetlite.app"
  else
    say "Run: meetlite-gui"
  fi
else
  say "Run: meetlite --help"
fi
