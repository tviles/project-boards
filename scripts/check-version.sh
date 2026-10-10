#!/bin/sh
# CI rule: Cargo.toml and herdr-plugin.toml carry the same version, and it is
# either a published release tag (v<version>) or ends in -dev.
set -eu
cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
manifest_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' herdr-plugin.toml | head -n 1)
if [ "$cargo_version" != "$manifest_version" ]; then
  echo "version mismatch: Cargo.toml $cargo_version, herdr-plugin.toml $manifest_version" >&2
  exit 1
fi
case "$cargo_version" in
  *-dev) echo "version $cargo_version (dev)"; exit 0 ;;
esac
if git tag -l "v$cargo_version" | grep -q .; then
  echo "version $cargo_version is released"
  exit 0
fi
echo "version $cargo_version is neither released (no tag v$cargo_version) nor -dev" >&2
exit 1
