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
- `join_disambiguation/`: parts of `test-join-disambiguation.sh`: derived handles, self-files, `declared_host.sh` (the declared host, the slot formatter and legacy-slot migration), `jq_args.sh`, pipe endpoints, send identity, slot guard, spawn and lock
- `lib-home-guard.sh`: the guard every suite sources first; drops the host's comm identity and daemon routes, and gives `guard_fresh_home`, `guard_refuse_live_home`, `guard_stage_bin`, `in_row`, and `guard_bridge_stub`, the suites' stand-in for `sotd stdio-bridge --endpoint`; every sotd it writes answers the Windows ancestry walk (`ancestors --from`) empty
- `lib-temp-files.sh`: what the two temp-file suites share: the jq stand-in that fails or hangs the call carrying a row's mark, the stand-in daemon, a run in a process group of its own, the held rm that keeps a cleanup trap's rm waiting while a row sends its signal a second time, a row's signal sent twice back to back, a row's own step once the daemon sees the silent op (`TF_BEFORE`), and the check that the temporary folders are empty
- `lib-wait.sh`: the waits the suites share: `await` (a check every 50 ms, a 30 s hang guard), `stopped` (a holder that stopped itself), `sleep_log` (the logging sleep that counts a command's waits), `end_group` (a background step started as a process group of its own ends whole, KILL after await's 30 s), `bounded` (GNU `timeout` on perl, which macOS has), `stamp_ago` (a UTC stamp N seconds old, without `date -d`) and `linux_only` (a case that tests Linux's inbox lock or reads /proc names itself skipped elsewhere)
- `stage-bin.sh`: validates the flat bin source and publishes each completed destination by exclusive sibling temporary and rename, without preserving source permissions
- `status_floor/`: parts of `test-status-floor.sh`: `reduction.sh`, `markers.sh`, `audit_and_races.sh`
- `test-agent-join.sh`: `comm-join.sh` declares the row's handle to its daemon over the owner endpoint, never the relay
- `test-agent-layers.sh`: a process acts as a handle only with at most one agent between it and its row's capsule
- `test-comm-deps.sh`: a missing jq, flock or perl is named by poll (in a FAILED line), session start and the Stop hook, never passed silently; without jq a send's and a leave's one line is FAILED naming it; a mail tool that is a perl module perl loads (Time::HiRes) is not missing
- `test-comm-e2e-readers.sh`: real readers on a real shared home lose no line (needs a v4 peer and a v3 host)
- `test-comm-matrix-verdict.sh`: the acceptance matrix's verdict logic tells a false failure from a false success
- `test-comm-private.sh`: every comm writer makes the comm folder its user's alone, an older folder is tightened at join, user files keep the caller's umask
- `test-comm-poll-cursor.sh`: the read cursor is a line offset that survives a torn line, a cut-back file and a legacy stamp
- `test-crlf-jq-output.sh`: the comm scripts compare handles correctly under a jq that writes CRLF
- `test-endpoint-gate.sh`: every endpoint value leaves `comm-lib.sh` through one gate; ssh resolvers and the wire round trip; no `sotd --socket` in another process's argv is ever an endpoint, and a process is asked for a socket only when its binary is named sotd
- `test-heartbeat-ctx-wait.sh`: guarded entry for the heartbeat context-deadline suite; runs the staged hook against finite context fixtures in its own temporary home
- `test-heartbeat-ctx-wait.py`: observes LF-byte fixtures/protocol, creator-returned artifact paths, hook exit plus both EOFs, actual release times and registry effects; MSYS budget fixtures and native P5 have separate readiness/lifetime cleanup evidence; the `trap-once` control (Unix) proves the responsive body records one cancellation under TERM storms
- `test-hub-files.sh`: the inbox append: one lock, both writers, fail closed, whole lines, routes and lock records
- `test-inbox-lock-onehost.sh`: the inbox lock on one machine whose mount lock is unknown (needs one peer host)
- `test-inbox-lock-twohost.sh`: concurrent shell and Rust appenders, frozen and killed holders across two boxes (needs a peer host)
- `test-join-disambiguation.sh`: derived handles are decided and written in one critical section; refusals and self-file healing
- `test-registry-io.sh`: the registry's one writer and one reader: unreadable is never absent, no write over a bad file; comm-list labels by the one heartbeat rule
- `test-registry-lock.sh`: the registry lock names its holder and is reclaimed only from a proven-dead holder
- `test-registry-lock-twohost.sh`: the registry lock's fresh read and distinct machine ids across boxes (needs peer hosts)
- `test-registry-twohost.sh`: registry writes on one box are read whole on another (needs peer hosts)
- `test-relay-file-first.sh`: a relay send ends in the appender's verdict, `not_here` included; a listed handle never touches the wire; `sot-nav.sh` sends one UI notification
- `test-rm-guard.sh`: the retained suite-bootstrap guard check and executed shared-await and `end_group` controls (the stub watcher's pipeline ends whole, a member that ignores TERM is killed, and the leader-only stop leaves the pipeline); removal and timing source catalogs are retired, with changed-entry cleanup observed by their owning suites
- `test-send-routes-to-relay.sh`: a registry miss goes to the wire; a listed handle that is not live is FAILED with nothing appended and no daemon asked, a live one files locally, never both; every script append, inbox redirection and `last_seen` file in the tracked tree is on a pinned list
- `test-stage-bin.sh`: complete old/new staged bytes, chosen modes, visible failures and owned-temp cleanup through absolute fixture programs
- `test-status-floor.sh`: the work-state reduction, its lifecycle through the hooks and the join's turn mark, closing markers and the turn auditor
- `test-temp-files.sh`: no messaging script or library call leaves a file in its temporary folders, on success, on failure, killed, or ended by TERM, HUP or INT, and when the signal comes twice, during the trap's rm or back to back; a Ctrl-C during `comm-relay.sh send` stops the shell that ran it; the heartbeat leaves no context capture file in the comm home's state when a signal ends it, and the registry lock and the Stop hook's feedback record go too

## Start here
`lib-home-guard.sh`, then the shortest suite (`test-agent-join.sh`) as the pattern: source the guard, make a work
directory, `guard_fresh_home`, `guard_stage_bin`, run the scripts from the stage.

## Rules
- Agent-layer input opens establish stderr redirection first; vanished process files preserve the existing chain/refusal result without a shell input-open diagnostic. The test table covers stat, cmdline and both winpid reads.
- The Codex off-hook fixture must prove no input read and no status call, as well as exit 0; unset/on retains blocked then stop.
- The two-host inbox Rust arm and the reader suite's one-shot wake control use scripts/tests/lib-test-body.sh to require the exact selected body to complete successfully; every caller consumes its checked result, including background controls. Existing required content and route witnesses remain additional assertions.
- Each suite sources `lib-home-guard.sh` before any command but `set`; `test-rm-guard.sh` fails a suite that does not,
  and fails if it finds fewer than 29 suites under `comm/` and `agents/`.
- Staging is atomic per file, not per bin generation. A failed copy, mode change or rename stops staging and leaves the existing public destination intact.
- Scripts run from the copy `guard_stage_bin` makes in the suite's work directory (`stage-bin.sh` fails on a missing or
  empty bin folder or a name two folders ship). Hooks and launchers run in-tree: the Stop hook runs
  `comm-turn-auditor.sh` when it sits beside it, so a suite that must not call a live model runs the hook from
  `comm/work_state/hooks`, where no auditor sits beside it.
- Five suites need peer hosts and are not hermetic: `test-comm-e2e-readers.sh`, `test-inbox-lock-onehost.sh`,
  `test-inbox-lock-twohost.sh`, `test-registry-twohost.sh` and `test-registry-lock-twohost.sh`.
- CI runs the hermetic list in `.github/workflows/rust.yml`; `scripts/tests/rc-gate.sh` runs every `test-*.sh` here but
  those five, and so does the job `bash32-linux`, with every script under bash 3.2 (`scripts/tests/under-bash.sh`). The
  suites of the row-lifecycle CLIs, the launchers and `sot-gh-auth.sh` are in `agents/tests/` and source this folder's
  guard.
- Code a suite hands to the bash on PATH (`bash -c`, a fixture script, a staged script) holds under bash 3.2, as the
  scripts do: in a `bash -c` shell the pid is `$$` (3.2 has no `$BASHPID`), and a case on a branch chosen by the bash
  version tests the branch the bash on PATH takes. The suite's own code runs on bash 4 or later.
- A shell rule that has a Rust twin is checked by a text scan or a parity test here (`test-hub-files.sh` T13 over
  `inbox.rs`, `test-registry-lock.sh` t15 over `lock.rs`); change both arms in one commit.
- A hermetic suite's verdict does not depend on how fast the host runs, down to the one speed bound left: the run's own
  timeout (`scripts/tests/rc-gate.sh` gives each suite 20 minutes, the CI job 60), which turns a hang into a failure. A
  case bounds a duration from above only to rule out a slower behaviour its exit status and output cannot show, and
  then by counting the code's own waits (`sleep_log`, lib-wait.sh). A wait the code under test enforces is set longer
  than every hang guard in the suite where the case can set it, unless its expiry is what the case tests (the registry
  lock's 10 s is fixed when the library is sourced, so status_floor's race() keeps it around one jq, mv and rmdir). A
  step a case starts in the background is awaited by a signal it gives (`await`; `stopped` for a holder that stops
  itself), never a fixed sleep, and ends before the case returns. A step that runs until it is stopped and has children of its own (a stub daemon's nc and watcher, a retired-bridge loop) starts between `set -m` and `set +m`, so it leads a process group, and ends with `end_group`, in its stop and in the suite's EXIT trap: a stop by parent id misses a pipeline's members, and Git Bash has no `pkill` or `pgrep`. (Steps that end themselves, such as a lock holder or a reader a case waits for, are outside this; `lib-temp-files.sh`'s `_tf_start` and `_tf_end` give each row's step a group of its own by `setpgrp`.) The trade: `set -m` takes the step out of the suite's own process group, so a developer run under `timeout -s KILL` (or `timeout -k`, once its grace after TERM has run out) leaves the stub's `nc`, watcher and `tail -F`, because the EXIT trap that ends them runs on TERM and not on KILL; the gate's container ends such leftovers. `test-comm-deps.sh`'s untethered case still asks `pgrep` for its loop's child, so that one row fails on Git Bash. A holder gives its ready signal itself (a builtin, such
  as `: > FILE`), never from a child that inherits the lock. Lower bounds ("waited at least the deadline") stay:
  load only lengthens a wait. The suites that need peer hosts and comm-matrix.sh time real boxes and are outside this
  rule. The repaired wait paths use observed readiness, retry/read-completion or child-result barriers; the two
  endpoint process waits require the recorded child's expected executable image. The intentional comm-deps and
  endpoint scenarios keep their result and completion assertions. test-rm-guard.sh runs `await` itself; no source
  count proves a wait policy.
- The heartbeat context-deadline suite validates frozen per-behavior observations, with separate fixture and exit/EOF failures. It publishes Bash fixtures and protocol as LF bytes, cleans creator-returned paths, and measures actual release/consumption times. Responsive budget bodies use Bash on every OS; native Python P5 remains separate. Sensitivity exercises the real assertions after readiness and parent-exit handshakes. Cleanup needs positive no-start or witnessed readiness plus lifetime completion, both EOFs, joined drains and accounted artifacts; missing readiness never certifies cleanup. Its independent observation deadline precedes separate finite-fixture cleanup, which never repairs a failed observation; the successful stamp control must run.
- The responsive body's TERM trap ignores TERM before it writes its one record, so the bound's second TERM (group, then the body) cannot run it again; `trap-once` sends the body TERM pairs every 10 µs, at most 200, 50 times, and requires one `cancelled` and one `completed` each time (an endless flood can overflow bash's own stack, so it is capped).
