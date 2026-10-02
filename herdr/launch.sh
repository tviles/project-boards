#!/bin/sh
# Single entrypoint for the pane and every action. herdr runs plugin commands with a
# minimal PATH, so add the usual locations; then find the installed (bin/) or linked
# (target/release/) binary and exec it with the given arguments.
export PATH="${PATH:-}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
root="${HERDR_PLUGIN_ROOT:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}"
bin="$root/bin/project-boards"
[ -x "$bin" ] || bin="$root/target/release/project-boards"
if [ ! -x "$bin" ]; then
  printf 'project-boards: binary not found; run `cargo build --release` in %s\n' "$root" >&2
  if [ "${1:-}" = "pane" ]; then
    printf 'press Enter to close\n'
    read -r _
  fi
  exit 1
fi
exec "$bin" "$@"
