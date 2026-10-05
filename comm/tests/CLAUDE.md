# comm/tests: the suites that prove the comm scripts (messaging)

One standalone bash suite per `test-*.sh`, each against its own temporary comm home, daemon stand-ins and process
stand-ins, never the live comm home. The scripts under test run from a staged flat copy of the bin folders in the repo's
form (the library's loader beside its parts; the installer puts each part inside the file that sources it, and
test/install_tests.jl shows comm-lib.sh defines the same functions and globals in both), so a script can move between `comm/lib` and
the other bin folders without editing a suite. Part of messaging; charter: comm/CLAUDE.md.

## Files
- `agent_layers/`: parts of `test-agent-layers.sh`: `table.sh` (the layer table and its parsers), `end_to_end.sh` (fake processes)
- `comm-matrix.sh`: the live acceptance matrix over real boxes (not a hermetic suite; run by hand at a release)
- `fixtures/`: the lock-identity cases (`inbox-lock-identity/`: mount tables, `cases.tsv`), read by `test-hub-files.sh` and the daemon's `inbox_tests.rs`
- `hub_files/`: parts of `test-hub-files.sh`: `lock_shell.sh`, `routes.sh`, `wire.sh`, `reader.sh`, `lock_faults.sh`
- `join_disambiguation/`: parts of `test-join-disambiguation.sh`: derived handles, self-files, `jq_args.sh`, pipe endpoints, send identity, slot guard, spawn and lock
- `lib-home-guard.sh`: the guard every suite sources first; drops the host's comm identity and daemon routes, and gives `guard_fresh_home`, `guard_refuse_live_home`, `guard_stage_bin`, `in_row`
- `stage-bin.sh`: lays the files of the folders in `comm/bin-folders.txt` flat into a destination, in the repo's form
- `status_floor/`: parts of `test-status-floor.sh`: `reduction.sh`, `markers.sh`, `audit_and_races.sh`
- `test-agent-join.sh`: `comm-join.sh` declares the row's handle to its daemon over the owner endpoint, never the relay
- `test-agent-layers.sh`: a process acts as a handle only with at most one agent between it and its row's capsule
- `test-comm-deps.sh`: a missing jq, flock or perl is named by poll, session start and the Stop hook, never passed silently
- `test-comm-e2e-readers.sh`: real readers on a real shared home lose no line (needs a v4 peer and a v3 host)
- `test-comm-matrix-verdict.sh`: the acceptance matrix's verdict logic tells a false failure from a false success
- `test-comm-private.sh`: every comm writer makes the comm folder its user's alone, an older folder is tightened at join, user files keep the caller's umask
- `test-comm-poll-cursor.sh`: the read cursor is a line offset that survives a torn line, a cut-back file and a legacy stamp
- `test-crlf-jq-output.sh`: the comm scripts compare handles correctly under a jq that writes CRLF
- `test-endpoint-gate.sh`: every endpoint value leaves `comm-lib.sh` through one gate; ssh resolvers and the wire round trip; no `sotd --socket` in another process's argv is ever an endpoint
- `test-heartbeat-ctx-wait.sh`: the heartbeat hook's wait on `comm-context.sh` polls fast, is bounded and cleans up
- `test-hub-files.sh`: the inbox append: one lock, both writers, fail closed, whole lines, routes and lock records
- `test-inbox-lock-onehost.sh`: the inbox lock on one machine whose mount lock is unknown (needs one peer host)
- `test-inbox-lock-twohost.sh`: concurrent shell and Rust appenders, frozen and killed holders across two boxes (needs a peer host)
- `test-join-disambiguation.sh`: derived handles are decided and written in one critical section; refusals and self-file healing
- `test-registry-io.sh`: the registry's one writer and one reader: unreadable is never absent, no write over a bad file; comm-list labels by the one heartbeat rule
- `test-registry-lock.sh`: the registry lock names its holder and is reclaimed only from a proven-dead holder
- `test-registry-lock-twohost.sh`: the registry lock's fresh read and distinct machine ids across boxes (needs peer hosts)
- `test-registry-twohost.sh`: registry writes on one box are read whole on another (needs peer hosts)
- `test-relay-file-first.sh`: the relay's ack means the frame is filed; a listed handle never touches the wire
- `test-rm-guard.sh`: every delete rooted in a variable is written `${VAR:?}`, and every comm and agents suite sources the guard first
- `test-send-routes-to-relay.sh`: a registry miss goes to the wire; a listed handle that is not live is FAILED with nothing appended and no daemon asked, a live one files locally, never both; every script append and every `last_seen` file is on a pinned list
- `test-status-floor.sh`: the work-state reduction, its lifecycle through the hooks, closing markers and the turn auditor

## Start here
`lib-home-guard.sh`, then the shortest suite (`test-agent-join.sh`) as the pattern: source the guard, make a work
directory, `guard_fresh_home`, `guard_stage_bin`, run the scripts from the stage.

## Rules
- Each suite sources `lib-home-guard.sh` before any command but `set`; `test-rm-guard.sh` fails a suite that does not,
  and fails if it finds fewer than 29 suites under `comm/` and `agents/`.
- Scripts run from the copy `guard_stage_bin` makes in the suite's work directory (`stage-bin.sh` fails on a missing or
  empty bin folder or a name two folders ship). Hooks and launchers run in-tree: the Stop hook runs
  `comm-turn-auditor.sh` when it sits beside it, so a suite that must not call a live model runs the hook from
  `comm/work_state/hooks`, where no auditor sits beside it.
- Five suites need peer hosts and are not hermetic: `test-comm-e2e-readers.sh`, `test-inbox-lock-onehost.sh`,
  `test-inbox-lock-twohost.sh`, `test-registry-twohost.sh` and `test-registry-lock-twohost.sh`.
- CI runs the hermetic list in `.github/workflows/rust.yml`; `scripts/tests/rc-gate.sh` runs every `test-*.sh` here but
  those five. The suites of the row-lifecycle CLIs, the launchers and `sot-gh-auth.sh` are in `agents/tests/` and source this folder's guard.
- A shell rule that has a Rust twin is checked by a text scan or a parity test here (`test-hub-files.sh` T13 over
  `inbox.rs`, `test-registry-lock.sh` t15 over `lock.rs`); change both arms in one commit.
