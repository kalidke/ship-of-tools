#!/usr/bin/env bash
# comm-send.sh — send a message to one session, or a copy to every session this box's registry lists.
# Usage: comm-send.sh @name 'message'
#        comm-send.sh @name --file <path | ->     (the body is the file's bytes, or stdin's)
#        comm-send.sh @name -                     (the body on stdin)
#        comm-send.sh --broadcast 'message'
#
# Quote a message in single quotes: inside double quotes a backquote or `$( )` runs as a command. A message holding
# a quote, or anything a shell reads, goes on stdin or in a file instead.
#
# The verdict is one line: `filed -> @name` (exit 0), the line is in the inbox of a handle a live session holds; or
# `FAILED -> @name: <why>` (exit 1), nothing was appended. This is `sotd comm send` (rust/backend/src/comm/CLAUDE.md).
. "$(dirname "${BASH_SOURCE[0]}")/comm-lib.sh" || exit 1
sot_comm send "$@"
