#!/usr/bin/env bash
# codex-watch.sh <handle> — wake for CODEX sessions (ADR 0031). Generalized
# into comm-wake.sh (ADR 0047: Claude sessions now wake the same way via
# `--deliver ping`, not a harness Monitor); this is the `full`-mode shim so
# `ccx` and every doc that already names this file keep working unchanged.
set -uo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/comm-wake.sh" "${1:-}" --deliver full
