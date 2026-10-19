#!/bin/sh
# Downloads GitHub's and Linear's public GraphQL schemas (SDL) into crates/queries/graphql/.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
dir="$root/crates/queries/graphql"
mkdir -p "$dir/linear"
curl -fsSL https://docs.github.com/public/fpt/schema.docs.graphql -o "$dir/schema.graphql"
echo "github schema: $(wc -c < "$dir/schema.graphql") bytes"
curl -fsSL https://raw.githubusercontent.com/linear/linear/master/packages/sdk/src/schema.graphql -o "$dir/linear/schema.graphql"
echo "linear schema: $(wc -c < "$dir/linear/schema.graphql") bytes"
