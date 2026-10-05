#!/usr/bin/env bash
# on-host.sh -- run a command in a folder on a second host without leaking either
# side's environment.
#
# Usage: on-host.sh HOST DIR -- CMD [ARG...]
#
# The remote shell reads a script from stdin (`ssh HOST bash -s`); no command
# string is built from local variables, so nothing local is expanded or split
# there. The script unsets every SOT_ variable, XDG_STATE_HOME, JULIA_LOAD_PATH
# and JULIA_PROJECT on the remote side, cd's to DIR, and execs CMD, whose words
# are quoted here with printf %q. Prints nothing of its own except refusals.

set -u

refuse() { echo "on-host.sh: $1 (usage: on-host.sh HOST DIR -- CMD [ARG...])" >&2; exit 2; }

[ $# -ge 2 ] || refuse "need HOST DIR -- CMD"
host=$1
dir=$2
shift 2
[ -n "$host" ] || refuse "HOST is empty"
[ -n "$dir" ] || refuse "DIR is empty"
[ "${1-}" = -- ] || refuse "missing --"
shift
[ $# -ge 1 ] || refuse "empty command"

printf -v qdir '%q' "$dir"
printf -v qcmd '%q ' "$@"

ssh "$host" bash -s <<SCRIPT
for v in \$(compgen -v SOT_) XDG_STATE_HOME JULIA_LOAD_PATH JULIA_PROJECT; do unset "\$v"; done
cd $qdir || { echo "on-host.sh: no such folder on the host: "$qdir >&2; exit 1; }
exec $qcmd
SCRIPT
