# comm/lib: the shared shell library (messaging)

`comm-lib.sh` is the one file every comm and agents script sources, from its own folder (`~/.sot-comm/bin` once
installed). In the repo it is a loader that sources seven parts beside it, one concept each; the installer installs it
as one file, each part's text in place of its `source` line (src/comm_bin.jl), so a script reads the whole library at
once and an install replaces it with one rename. Both forms give a caller the same functions and globals. Part of
messaging; charter: comm/CLAUDE.md.

## Files
- `comm-lib.sh`: the loader; sources the seven parts below, in this order, and holds nothing else
- `comm-lib-base.sh`: the platform test, the comm folder's paths (`COMM_HOME`, `REGISTRY`, the lock path), the clock, tool checks, jq and host helpers, ages
- `comm-lib-client.sh`: the shell client of the daemon's wire: endpoints, the ssh bridge, the hello frame, `sot_oneshot_request`, pty input and screen
- `comm-lib-registry-lock.sh`: the registry lock: `with_lock` and the lock record's take, judge and fail steps
- `comm-lib-registry.sh`: the registry file: `ensure_home`, the writers, the reads and a row's status
- `comm-lib-inbox.sh`: the inbox append and its lock, `sot_comm_file`, the read cursor and the line counts
- `comm-lib-identity.sh`: the self file, the routable-identity gate, slugs and derived handles (`claim_derived_handle`)
- `comm-lib-agent-layers.sh`: the agent-layer check (`sot_require_agent`): which agents lie between a script and its row

`comm-lib-client.sh` is the agents subsystem's code (agents/CLAUDE.md), housed here because the library calls it
(`sot_comm_file` reaches the daemon through `sot_oneshot_request`).

## Start here
By concept: `sot_inbox_append` for mail, `registry_replace` and `sot_registry_read` for the registry, `with_lock` for its
lock, `claim_derived_handle` for a derived handle, `sot_require_agent` for who may act as a handle,
`sot_oneshot_request` for a request to the daemon.

## Rules
- `comm-lib.sh` only sources its parts, and a part calls nothing while it is sourced: outside function bodies there are
  assignments only, so the order of the parts changes no behaviour. A new part is a new file here and a new line
  `source "$(dirname "${BASH_SOURCE[0]}")/<part>" || return 1` there, the form the installer inlines (a `source` or `.`
  command it sees anywhere else whose path names a part stops the install; it reads lines, not bash, so a command in a
  case arm, after an assignment or a command word, split over lines or in process substitution, a path built from a
  variable and a part run by another command are not seen). A part sources nothing, and uses no `BASH_SOURCE`, no `LINENO` and no top-level
  `return`, so inlined it defines the same functions and globals.
- One registry write (`registry_replace` under `with_lock`) and one read (`sot_registry_read`: 0 present, 1 absent, 2
  unreadable).
- One inbox append (`sot_inbox_append`) and one `comm.file` request (`sot_comm_file`).
- Every endpoint leaves through `_sot_emit_endpoint`.
- A value that may start with `/` goes through `sot_jq_rawfile`, never `jq --arg`.
- A rule written in both shell and Rust changes in both in one commit (the lock record, the cursor, the lock identity).
- The host part `comm-context.sh` gives an unpinned self file, and `comm-despawn.sh`'s match of it, come from `sot_raw_host`
  (raw `hostname -s`, case kept, a non-empty `SOT_COMM_TEST_HOST` first), not `sot_host`. A capsule's pinned self file is
  named by the daemon with its declared host, which is `sot_host`'s rule.
- bash 3.2 and git-bash.
- The installer publishes this folder before every script (comm/bin-folders.txt), so during an install the previous
  release's scripts source this library: a release removes or changes a function or global only once no script of the
  previous release uses it.
