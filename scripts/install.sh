#!/bin/sh
# herdr [[build]] step. Downloads the release binary that matches the manifest
# version and verifies its SHA-256; builds from source for -dev versions or when
# no release exists. A checksum mismatch aborts the install.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export PATH="${PATH:-}:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/herdr-plugin.toml" | head -n 1)
base="${PB_RELEASE_BASE:-https://github.com/tviles/project-boards/releases/download}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-musl ;;
  *) echo "project-boards: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
asset="project-boards-$target"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }

build_from_source() {
  echo "project-boards: building from source (cargo build --release)"
  (cd "$root" && cargo build --release)
  mkdir -p "$root/bin"
  cp "$root/target/release/project-boards" "$root/bin/project-boards"
  chmod 0755 "$root/bin/project-boards"
}

case "$version" in
  *-dev) build_from_source; exit 0 ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pb-XXXXXX")
trap 'rm -rf "$tmp"' EXIT
if curl -fsSL -o "$tmp/$asset" "$base/v$version/$asset" \
  && curl -fsSL -o "$tmp/$asset.sha256" "$base/v$version/$asset.sha256"; then
  expected=$(cut -d ' ' -f 1 < "$tmp/$asset.sha256")
  actual=$(sha "$tmp/$asset" | cut -d ' ' -f 1)
  if [ "$expected" != "$actual" ]; then
    echo "project-boards: checksum mismatch for $asset (expected $expected, got $actual)" >&2
    exit 1
  fi
  mkdir -p "$root/bin"
  cp "$tmp/$asset" "$root/bin/project-boards"
  chmod 0755 "$root/bin/project-boards"
  echo "project-boards: installed release v$version for $target"
else
  echo "project-boards: no release binary for v$version ($target)"
  build_from_source
fi
