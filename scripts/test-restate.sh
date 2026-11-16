#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root. GNU timeout is `gtimeout` on macOS (coreutils).
target=${1:-request_owner}
case "$target" in
  request_owner|canonical_link|durable_projection|retained_delivery|approved_delivery|installation_availability|installation_audit_replay) ;;
  authoritative_admission)
    # Its tests each deploy their own endpoint (fault handles, storage) under
    # the same service names; one shared server routes to whichever registered
    # last, and retained state from earlier tests leaks into later ones.
    printf '%s\n' 'authoritative_admission needs a Restate server per test, not this shared runtime. Run:' \
      '  RESTATE_SERVER_BIN=/path/to/restate-server cargo test --locked -p ghinvite-workflows --features integration --test authoritative_admission -- --test-threads=2' \
      'See docs/local-testing.md.' >&2
    exit 2 ;;
  *) printf '%s\n' 'Usage: bash scripts/test-restate.sh [request_owner|canonical_link|durable_projection|retained_delivery|approved_delivery|installation_availability|installation_audit_replay]' >&2; exit 2 ;;
esac
export RESTATE_PROOF_CLEANUP_INTERVAL=1s
deadline=$(command -v timeout || command -v gtimeout) || {
  printf '%s\n' 'Restate acceptance FAILED: GNU timeout (coreutils) is required.' >&2
  exit 1
}
# Escalate if a child ignores TERM, so every shell deadline is a hard bound.
bounded() { "$deadline" --kill-after=5s "$@"; }
command -v docker >/dev/null || {
  printf '%s\n' 'Restate acceptance FAILED: Docker is required; no runtime verification performed.' >&2
  exit 1
}
bounded 15s docker info >/dev/null
bounded 15s docker compose version

# Compile before starting infrastructure; respect rust-toolchain.toml and Cargo.lock.
rustc --version
bounded 600s cargo test --locked -p ghinvite-workflows --features integration --test "$target" --no-run

project="ghinvite-smoke-$(date +%s)-$$-$RANDOM"
compose=(docker compose -f compose.yaml --profile smoke -p "$project")
cleanup() {
  status=$?
  trap - EXIT
  if (( status != 0 )); then
    printf '%s\n' 'Restate acceptance FAILED. Bounded diagnostics from this disposable runtime:' >&2
    bounded 10s "${compose[@]}" ps -a >&2 || true
    bounded 10s "${compose[@]}" logs --no-color --tail 60 restate-smoke >&2 || true
  fi
  if ! bounded 30s "${compose[@]}" down --volumes --remove-orphans --timeout 5; then
    printf 'Cleanup failed; retry: docker compose -f compose.yaml -p %s down --volumes\n' "$project" >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

bounded 180s "${compose[@]}" up -d restate-smoke
admin=$(bounded 10s "${compose[@]}" port restate-smoke 9070)
ingress=$(bounded 10s "${compose[@]}" port restate-smoke 8080)
export RESTATE_ADMIN_URL="http://$admin"
export RESTATE_INGRESS_URL="http://$ingress"
export RESTATE_ENDPOINT_HOST=host.docker.internal
bounded 150s cargo test --locked -p ghinvite-workflows --features integration --test "$target" -- --nocapture
