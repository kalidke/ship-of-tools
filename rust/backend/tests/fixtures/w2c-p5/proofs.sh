#!/usr/bin/env bash
# Hosted-only premise probe; this never launches a manager on a lab machine.
set -euo pipefail
umask 022
fixture_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec python3 "$fixture_dir/probe.py" "$@"
