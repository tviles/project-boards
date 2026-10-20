#!/bin/sh
# Prints the body of one version's section of CHANGELOG.md (the lines under
# "## [<version>]", without the heading, the next section or the compare links), for
# release notes. Exits 1 when the changelog has no section for that version.
# Usage: sh scripts/changelog-section.sh 0.4.0   (or Unreleased)
set -eu
version=${1:-}
changelog=${CHANGELOG:-CHANGELOG.md}
if [ -z "$version" ]; then
  echo "usage: $0 <version>" >&2
  exit 1
fi
awk -v heading="## [$version]" '
  # The heading is "## [<version>]", alone or followed by " - <date>".
  index($0, "## [") == 1 {
    if (found) exit
    if ($0 == heading || index($0, heading " ") == 1) { found = 1; next }
  }
  found && /^\[[^]]*\]: / { exit }
  found {
    if ($0 ~ /^[[:space:]]*$/) { blank++; next }
    if (printed) for (i = 0; i < blank; i++) print ""
    blank = 0; printed = 1; print
  }
  END { if (!found) exit 1 }
' "$changelog" || {
  echo "$changelog has no section for $version" >&2
  exit 1
}
