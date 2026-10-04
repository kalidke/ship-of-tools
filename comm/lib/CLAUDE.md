# comm/lib: the shared shell library (messaging)

`comm-lib.sh` is the one file every comm and agents script sources, from its own folder (flat once installed in
`~/.sot-comm/bin`). It holds the comm folder's paths, the registry and its lock, the inbox append and the read cursor,
identity, the agent-layer check and the shell client of the daemon's wire. Part of messaging; charter: comm/CLAUDE.md.

## Files
- `comm-lib.sh`: the whole library, sourced, never executed; it defines functions and globals and runs nothing

## Start here
`sot_inbox_append` for mail, `registry_replace` and `sot_registry_read` for the registry, `with_lock` for its lock,
`claim_derived_handle` for a derived handle, `sot_require_agent` for who may act as a handle, `sot_oneshot_request` for a
request to the daemon.

## Rules
- Nothing outside a function body calls a function while the file is sourced; only assignments run.
- One registry write (`registry_replace` under `with_lock`) and one read (`sot_registry_read`: 0 present, 1 absent, 2
  unreadable).
- One inbox append (`sot_inbox_append`) and one `comm.file` request (`sot_comm_file`).
- Every endpoint leaves through `_sot_emit_endpoint`.
- A value that may start with `/` goes through `sot_jq_rawfile`, never `jq --arg`.
- A rule written in both shell and Rust changes in both in one commit (the lock record, the cursor, the lock identity).
- bash 3.2 and git-bash.
