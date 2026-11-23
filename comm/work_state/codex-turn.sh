# Sourced only by Codex hooks. Keep opaque ids as a JSON pair, including any punctuation.
SOT_CODEX_TURN=$(jq -cse 'select(length == 1) | .[0] | if .agent_id != null then 0 else
    [.session_id,.turn_id] | select(all(.[]; type=="string" and length>0)) end' 2>/dev/null) || SOT_CODEX_TURN=
SOT_CODEX_ROOT=1
[ "${SOT_CODEX_TURN%$'\r'}" != 0 ] || { SOT_CODEX_ROOT=; SOT_CODEX_TURN=; }
export SOT_CODEX_TURN
