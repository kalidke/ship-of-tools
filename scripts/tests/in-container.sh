#!/usr/bin/env bash
# in-container.sh UNIT -- CMD [ARG...]: today's lane-job shape (a scope), replaced in the next commit.
set -u
[ $# -ge 3 ] && [ "$2" = -- ] || { echo "usage: in-container.sh UNIT -- CMD [ARG...]" >&2; exit 2; }
unit=$1; shift 2
exec systemd-run --user --scope --quiet --unit="$unit" -- "$@"
