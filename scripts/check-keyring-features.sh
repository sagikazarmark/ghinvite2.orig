#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for target in x86_64-unknown-linux-gnu wasm32-unknown-unknown; do
  graph=$(cargo tree --locked --target "$target" -p ghinvite-web-worker -p ghinvite-web-server -e normal,build,features -i anykms-keyring)
  printf '%s\n' "$graph"
  if [[ "$graph" == *'feature "raw-kek"'* ]]; then
    echo 'raw-kek must not be enabled in P1 application targets' >&2
    exit 1
  fi
done
