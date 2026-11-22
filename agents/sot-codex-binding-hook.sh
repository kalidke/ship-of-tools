#!/usr/bin/env bash
# Publish the root thread named by this row's hook. The host writes the whole event even when we skip it.
exec >/dev/null 2>&1
skip() { cat; exit 0; }
file=${SOT_CODEX_THREAD_FILE:-}
[ -n "$file" ] && [ "${SOT_AGENT_KIND:-}" = codex ] && [ -n "${SOT_WORKSPACE_ID:-}" ] || skip
[ "${SOT_COMM_HOOKS:-}" != off ] || skip
. "$(dirname "$0")/comm-lib.sh" || skip
# Compare the capsule with the leg's workspace id even if comm points to another row's self file.
SOT_COMM_SELF_FILE= sot_require_agent </dev/null || skip
id=$(jq -ers 'select(length == 1) | .[0].session_id | strings |
    select(length == 36 and test("^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$"; "i"))') || skip

tmp=
trap 'if [ -n "$tmp" ]; then rm -f -- "$tmp"; fi' EXIT
trap 'exit 0' HUP INT TERM
tmp=$(mktemp "$file.XXXXXX") || exit 0
printf '%s\n' "$id" > "$tmp" || exit 0
mv -f -- "$tmp" "$file" || exit 0
