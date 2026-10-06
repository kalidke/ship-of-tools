# comm/lib: the shared shell library (messaging)

`comm-lib.sh` is the one file every comm and agents script sources, from its own folder (`~/.sot-comm/bin` once
installed). In the repo it is a loader that sources seven parts beside it, one concept each; the installer installs it
as one file, each part's text in place of its `source` line (src/comm_bin.jl), so a script reads the whole library at
once and an install replaces it with one rename. Both forms give a caller the same functions and globals. Part of
messaging; charter: comm/CLAUDE.md.

## Files
- `comm-lib.sh`: the loader; sources the seven parts below, in this order, and holds nothing else
- `comm-lib-base.sh`: the platform test, the comm folder's paths (`COMM_HOME`, `REGISTRY`, the lock path), the clock, tool checks, the command bound (`sot_bounded`), jq and host helpers, ages, and `COMM_LIVE_SECS` (how old a `last_seen` may be and still be live)
- `comm-lib-client.sh`: the shell client of the daemon's wire: endpoints, the ssh bridge, the hello frame, `_sot_os_user` (this shell's OS account, on Windows the SID `_sot_windows_sid` reads), `sot_oneshot_request`, pty input and screen
- `comm-lib-registry-lock.sh`: the registry lock: `with_lock` and the lock record's take, judge and fail steps
- `comm-lib-registry.sh`: the registry file: `ensure_home`, the writers, the reads, a row's status and `sot_heartbeat_fresh` (is a `last_seen` live)
- `comm-lib-inbox.sh`: the inbox append and its lock, `sot_comm_file`, the read cursor, the line counts and `sot_unread` (the unread count)
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
  assignments only, and comm-lib-base.sh's `umask 077`, which reads nothing, so the order of the parts changes no
  behaviour. A new part is a new file here and a new line
  `source "$(dirname "${BASH_SOURCE[0]}")/<part>" || return 1` there, the form the installer inlines (a `source` or `.`
  command it sees anywhere else whose path names a part stops the install; it reads lines, not bash, so a command in a
  case arm, after an assignment or a command word, split over lines or in process substitution, a path built from a
  variable and a part run by another command are not seen). A part sources nothing, and uses no `BASH_SOURCE`, no
  `LINENO` and no top-level `return`, so inlined it defines the same functions and globals.
- Every script that sources the library writes under umask 077, except `comm-worktree-new.sh`, which sets the caller's
  mask back for the worktree. The comm folder's path is made absolute once, in comm-lib-base.sh, so a later `cd` or an
  exported `CDPATH` cannot make two commands mean two folders: a relative `SOT_COMM_HOME` is resolved against the first
  comm script's directory and exported absolute, so its children agree (the daemon resolves it against its own).
- `ensure_home` (`_sot_comm_tighten`) removes group and other permissions from the layout's own entries only
  (`_sot_comm_own` names them: the folder, `inbox/`, `read/`, `self/`, `state/`, `probe/` and its folders,
  `registry.json`, `registry.json.tmp` and `registry.json.new.*`, `.registry.lock` and its `.tmp.*` and `.reclaim.*`
  files, the lock manager's record and temp, `gh-device-auth.json`, and the files of those folders: comm/PROTOCOL.md's
  layout block), and only while the comm folder itself is open to group or other. It hands `find` the
  layout folders and lets it descend one level, so a layout folder that is a symlink is never followed, and it closes
  the comm folder last. An unknown file or folder, `bin/` and `VERSION` keep their modes behind the 0700 folder. A file
  `chmod` cannot change (another account's) is warned about once, at the pass that closes the folder, and not again.
  `comm-join.sh` runs it at every join.
- The refusal is a deny list: a comm folder that is the root, the home folder or a git checkout gets one warning and
  its permissions are not changed (the folders `ensure_home` makes are still made). A mistaken `SOT_COMM_HOME` that
  names any other folder still loses its own group and other bits, and those of its children the list names, because
  nothing marks a folder as a comm folder before `ensure_home` writes the registry in it. A warning of a failed
  tightening comes only from a re-check of the end state, naming the entry still open.
- One registry write (`registry_replace` under `with_lock`) and one read (`sot_registry_read`: 0 present, 1 absent, 2
  unreadable).
- One inbox append (`sot_inbox_append`), one `comm.file` request (`sot_comm_file`) and one unread count (`sot_unread`,
  the wake's rule; a count that cannot be made returns 1, never 0).
- One rule for a live `last_seen` (`sot_heartbeat_fresh`, `COMM_LIVE_SECS`), the twin of the filer's `heartbeat_fresh`.
- Every endpoint leaves through `_sot_emit_endpoint`.
- Every connection the library and its callers open goes through `sot_dial` or `sot_ssh_bridge`: a `unix:` or
  `pipe:` endpoint through `sotd stdio-bridge --endpoint`, which connects only to an endpoint this OS account serves,
  and an `ssh:` one through `sot_ssh_bridge`, whose far end is that box's own bridge (ADR 0049 `## User isolation`).
  A caller keeps the bridge's input open until it has read what it waits for.
- A value that may start with `/` goes through `sot_jq_rawfile`, never `jq --arg`.
- A rule written in both shell and Rust changes in both in one commit (the lock record, the cursor, the lock identity, the unread count, the heartbeat).
- The host part `comm-context.sh` gives an unpinned self file, and `comm-despawn.sh`'s match of it, come from `sot_raw_host`
  (raw `hostname -s`, case kept, a non-empty `SOT_COMM_TEST_HOST` first), not `sot_host`. A capsule's pinned self file is
  named by the daemon with its declared host, which is `sot_host`'s rule.
- bash 3.2 and git-bash.
- The four timed comm calls run under `sot_bounded` (comm-lib-base.sh), never under `timeout`: `sot_ssh_bridge`'s `ssh`,
  `sot_dial`'s bridge, comm-list.sh's `sot-fe version` and comm-turn-auditor.sh's headless claude. One perl process owns
  the deadline and the command's process group until the command has exited and no member of the group is left, so a
  descendant still holding the output after the command exits is ended at the bound too, and the call's status is then
  the bound's, with the command's own on stderr. At the bound, or when that perl is itself sent TERM, INT or HUP, it
  signals the group and the command itself, KILLs them a second later by the clock if any is left, and returns a second
  after that in any case, naming on stderr what still ran; the bound holds when the caller is killed. With no perl, no
  process group or a bound that is not a whole number above 0, the call does not run (125). Outside it: a descendant
  that leaves the command's group (setsid, as ssh's ControlPersist master does), and on Windows a native program and its
  children (`sot_dial`'s sotd.exe, the auditor's claude), since Git Bash emulates the group and its signals for its own
  programs only, and the tests run only those. The PostToolUse heartbeat (hooks/comm-status-heartbeat.sh) keeps its own
  watchdog over comm-context.sh: TERM at its bound, then a wait, and the output discarded.
- The installer publishes this folder before every script (comm/bin-folders.txt), so during an install the previous
  release's scripts source this library: a release removes or changes a function or global only once no script of the
  previous release uses it.
- A request sent behind its hello is decided by its own reply. `sot_oneshot_request` stops at a refused hello at once only
  when the code is not `protocol_mismatch` (a daemon of this release closes after refusing, and only it sends the other
  codes; an older daemon refuses only for the protocol and still answers the request), and names the refusal (`hello
  refused: <words>`) only when no reply came (`_sot_hello_refusal`).
