#!/bin/sh
# Downloads GitHub's public GraphQL schema (SDL) into crates/queries/graphql/schema.graphql.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
dir="$root/crates/queries/graphql"
mkdir -p "$dir"
curl -fsSL https://docs.github.com/public/fpt/schema.docs.graphql -o "$dir/schema.graphql"
echo "schema: $(wc -c < "$dir/schema.graphql") bytes"
