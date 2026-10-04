# comm/: messaging between sessions (charter)

## Idea
A session sends one line to another by handle (the folder name plus the box name). The message is delivered when that
line is appended, under the inbox lock, to `inbox/<handle>.jsonl` in the comm folder (`~/.sot-comm`, or
`$SOT_COMM_HOME`); the sender is told `filed -> @h` or `FAILED -> @h: <reason>`, and nothing is queued. The daemon that
runs the receiver's row types a wake line into its free prompt, and the Stop hook does not end a turn with directed mail
unread. Design of record: docs/adr/0049-messaging-on-one-page.md. The contract is `PROTOCOL.md`.

## Owns
- The comm folder's data: `inbox/<h>.jsonl` and `.lock`, `read/<h>.cursor`, `registry.json` and `.registry.lock`,
  `self/`, `state/`, and `inbox-lock-manager` (written only by the folder's hub, at its start).
- Mail scripts: `comm-send.sh`, `comm-relay.sh`, `comm-poll.sh` (the only cursor writer), and the inbox half of
  `comm-lib.sh` (`sot_inbox_append`, `sot_comm_file`).
- Address book scripts: `comm-context.sh`, `comm-join.sh`, `comm-leave.sh`, `comm-list.sh`, `comm-self-audit.sh`,
  `comm-registry-lock-clear.sh`, and `comm-lib.sh`'s registry half (`registry_replace`, `sot_registry_read`, `with_lock`,
  `sot_require_agent`).
- Work-state: `comm-status.sh`, `comm-turn-auditor.sh`, `comm-session-start.sh`, the four Claude status hooks and the
  Codex blocked hook (the row colour is a reduction of the facts they stamp).
- Housed here, not messaging: the shell daemon client in `comm-lib.sh` (`sot_oneshot_request`) and the agent adapters under
  `adapters/`. The CLIs that start, end, probe and bootstrap rows moved to `agents/spawn/`, the `/worktree` scripts to
  `agents/worktree/`, and `sot-fe` and `sot-nav.sh` to `agents/sot-fe/`, installed into the same bin. The launchers, the
  non-messaging skills, `sot-gh-auth.sh` and `comm-pipe-request.ps1` moved to `agents/`.

## Promises
- `filed` is printed only on the appender's word: a local append that synced, a daemon's `comm.file` answer `ok`, or a
  filer's receipt carrying this send's id. `FAILED` appended nothing.
- Every appender takes flock on `inbox/<h>.lock` and appends locally only when its own lock manager equals line 1 of
  `inbox-lock-manager`; otherwise the frame goes to a daemon as `comm.file`.
- Readers count newline-terminated lines only; only `comm-poll.sh` moves a cursor.
- Every registry write is `registry_replace` under `with_lock` (scripts) or `with_comm_registry_lock` (daemon).
- A process acts as a handle only if at most one agent lies between it and its row's capsule (`sot_require_agent`).
- A rule written in both shell and Rust changes in both in one commit; parity tests cover the cursor, the lock record
  and the lock identity.
- Scripts run under bash 3.2 and git-bash; a value that may start with `/` never goes through `jq --arg`
  (`sot_jq_rawfile`).
- A change to locking states its writer set and is proven by concurrent appends from two hosts to one inbox; there is
  no lease-lock fallback. A script change is tested through `tests/` from the staged bin, never against the live home.
- Not built: ADR 0049's one row per handle (its stages B5 and B6). `set_agent_handle` clears no other row and
  `comm-join.sh` still accepts `--name`, so the wake skips a handle that two rows declare.

## Connections
- In: the capsule's env at spawn (`SOT_COMM_NAME`, `SOT_COMM_HOME`, `SOT_COMM_SELF_FILE`); `agent.join` from
  `comm-join.sh`; `comm.file` from `comm-send.sh` and `comm-relay.sh`; the Stop hook's `stop_at` mark.
- Out: the wake types through the capsule's headless attach (`wake_if_free`); a guest daemon forwards `comm.file` to
  the hub (`forward_comm_file`); the hub link reaches the hub over the topology's ssh recipe.
- The installer (`install_comm` in `src/install.jl`, folders read by `src/comm_bin.jl`) copies every file of the folders
  listed in `bin-folders.txt` flat into `~/.sot-comm/bin`; a script change counts once installed (`update_comm`).
- The work-state reaches the frontend only through `workspace.list`.

## Folders
- `adapters/`: what is installed into Claude Code and Codex: hooks, the messaging skills, the Codex skills and plugin.
- `core/`: the reference client's scripts, `core/scripts/`, each sourcing `comm-lib.sh` from its own folder.
- `mail/`: `comm-send.sh`, `comm-relay.sh` and `comm-poll.sh`, installed flat beside the core scripts.
- `registry/`: the address book scripts: identity, join, leave, list, self-audit, lock recovery, session start.
- `tests/`: the hermetic suites that prove the scripts, run from a staged flat bin (see its page).
- `work_state/`: `comm-status.sh`, `comm-turn-auditor.sh` and the status hooks (`work_state/hooks/`), the row colour's reduction.
- `rust/backend/src/comm/mail/`: the daemon's delivery: `comm.file` (`file_comm`), `agent.send` and `agent.filed`
  (`relay.rs`), the `agent.message` and `agent.receipt` buses, the hub link (`run`), and the append (`file_frame`).
- `rust/backend/src/comm/registry/`: the daemon's address book: `agent.join` (`handle_agent_join`), the registry lock
  (`acquire`), reads, the destroy prune and `clear_comm_unread`.
- `rust/backend/src/comm/wake/`: the wake tick (`check_row`), the screen reading and the one attach.

## Files
- `PROTOCOL.md`: the contract every client implements: layout, registry, inbox, cursor, locks, the receipt rule.
- `adapters/`: Claude and Codex adapters (see Folders).
- `bin-folders.txt`: the folders whose files install flat into `~/.sot-comm/bin`, one repo path per line.
- `core/`: the reference client (see Folders).
- `mail/`: the send, relay and poll scripts (see Folders).
- `registry/`: the address book scripts (see Folders).
- `tests/`: the suites and their stage (see Folders).
- `work_state/`: the work-state scripts and hooks (see Folders).

## Start here
For mail, `mail/comm-send.sh` then `sot_inbox_append` in `comm-lib.sh`, and `rust/backend/src/comm/mail/inbox.rs`
`file_frame` for the daemon's twin. For the address book, `comm-join.sh` and `registry_replace`. For work-state,
`comm-status.sh`. For a change to where a script lives, `bin-folders.txt` and `tests/stage-bin.sh`.
