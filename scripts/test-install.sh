#!/bin/sh
# Tests scripts/install.sh against fake file:// releases and a fake cargo.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/pb-XXXXXX")
trap 'rm -rf "$work"' EXIT

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-musl ;;
  *) echo "unsupported test host"; exit 1 ;;
esac
asset="project-boards-$target"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }

# A fake cargo that "builds" by writing a marker binary.
mkdir -p "$work/fakebin"
cat > "$work/fakebin/cargo" <<'EOT'
#!/bin/sh
mkdir -p target/release
printf '#!/bin/sh\necho built-from-source\n' > target/release/project-boards
chmod +x target/release/project-boards
EOT
chmod +x "$work/fakebin/cargo"

make_root() { # $1 = dir, $2 = version
  mkdir -p "$1/scripts"
  cp "$here/install.sh" "$1/scripts/install.sh"
  printf 'id = "tviles.project-boards"\nversion = "%s"\n' "$2" > "$1/herdr-plugin.toml"
}
make_release() { # $1 = releases dir, $2 = version, $3 = binary body
  mkdir -p "$1/v$2"
  printf '#!/bin/sh\necho %s\n' "$3" > "$1/v$2/$asset"
  (cd "$1/v$2" && sha "$asset" > "$asset.sha256")
}
fail() { echo "FAIL: $1"; exit 1; }

# 1. A matching release with a good checksum installs the downloaded binary.
make_root "$work/r1" 1.2.3
make_release "$work/rel" 1.2.3 downloaded
PATH="$work/fakebin:$PATH" PB_RELEASE_BASE="file://$work/rel" sh "$work/r1/scripts/install.sh"
[ "$("$work/r1/bin/project-boards")" = downloaded ] || fail "release install"

# 2. A checksum mismatch aborts with a non-zero exit and installs nothing.
make_root "$work/r2" 1.2.3
cp -R "$work/rel" "$work/badrel"
echo "0000  $asset" > "$work/badrel/v1.2.3/$asset.sha256"
if PATH="$work/fakebin:$PATH" PB_RELEASE_BASE="file://$work/badrel" sh "$work/r2/scripts/install.sh"; then
  fail "checksum mismatch should fail"
fi
[ ! -e "$work/r2/bin/project-boards" ] || fail "nothing installed on mismatch"

# 3. A -dev version builds from source.
make_root "$work/r3" 1.3.0-dev
(cd "$work/r3" && PATH="$work/fakebin:$PATH" PB_RELEASE_BASE="file://$work/rel" sh scripts/install.sh)
[ "$("$work/r3/bin/project-boards")" = built-from-source ] || fail "dev build"

# 4. A version with no release falls back to building from source.
make_root "$work/r4" 9.9.9
(cd "$work/r4" && PATH="$work/fakebin:$PATH" PB_RELEASE_BASE="file://$work/rel" sh scripts/install.sh)
[ "$("$work/r4/bin/project-boards")" = built-from-source ] || fail "missing release fallback"

echo "install.sh: all 4 cases passed"
