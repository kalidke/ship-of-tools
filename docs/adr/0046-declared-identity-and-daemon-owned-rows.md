# ADR 0046: Declared identity, daemon-owned rows, and the resident attach

**Status:** Accepted — owner-approved plan (2026-09-13); decision text through three Codex rounds (scope cut after round 2: the work-state reducer port and the launch-recipe file deferred to sprint 2); lanes in progress. Families A, B, D and the now-part of E of the merged elegance audit (ops sidecar, `audits/2026-09-13-elegance-audit-merged.md`); the switch-latency paper's option A. Families C, F, G, H and the tmux deletion are a second sprint.
**Date:** 2026-09-13

Amends ADR 0010, 0023, 0025 (§2, §4, the caption update, `fe_handle`),
0028 (handle sanitising), 0041 (attach contract; owner-emitted events),
0042 (FE-driver decisions 1–3), 0043 (decisions 23, 35), 0045 (decisions
1, 2; its one-migration mechanism reused).

## Context

**The exemplar.** On 2026-09-13 a backend session could not name the
frontend box it was talking to, while the daemon's roster listed that box
by handle: the "frontend connected" log line (`clients.rs:148-152`)
dropped the `fe_handle` the frontend had declared. Around it, five
host-name resolvers with five answers (`gpu.rs:19139`, `handlers.rs:284`,
`workspaces.rs:1110`, `comm-lib.sh:884`, `hosts.rs:43`); a Linux frontend
derives `win-fe-unknown`. Two independent audits (24 findings) found one
shape everywhere: a consumer computed a fact at the point of need instead
of the owner declaring it once, and each wrong computation was fixed with
a resolver tier or an override env, never a declaration.

**The switching requirement.** Owner: 1 s is not acceptable, 100 ms is,
the target is 10 ms. Measured at rc.24 over a ~70 ms link:

| Case | switch → content visible |
|---|---|
| capsule row on the backend host (8 switches, 6 rows) | 873–995 ms |
| capsule row on the frontend box itself | ~227 ms |
| tmux row on the backend host | no wait: the pty already streams over the open control connection |

A remote capsule switch is nine ordered round trips and probes the
supervisor twice. One round trip is seven times the target, so the
terminal's first paint may wait for no network operation: the row's
screen must already be resident and live. The tmux stream, measured on a
live row: essentially zero at rest, a median active second of 2.3 KB/s,
mean 140 KB/s, peak 3.4 MB/s in a build burst.

## The decision in one paragraph

A process declares who it is once, at hello, and every log line and
roster surfaces that declaration — never a routing key, and never an
on-disk address, this sprint. The daemon that spawned a row owns the
row's facts — phase, the agent's handle, the latest result, the record
location — observed or declared to it, persisted, served from memory. The
daemon keeps a watcher on every ready capsule row and relays the voyage's
own frames to every frontend over the control connection, as tmux output
flows today, so the terminal's first paint after a switch waits for no
network operation. The launch recipe has one owner, the daemon's
`agent_argv`, which the shell launchers exec through. Nothing new runs on
tmux; a capsule-capable install needs no tmux.

## Decisions

1. **A connection declares its identity on the wire, for display and
   `agent.join` only — no address or on-disk namespace change this
   sprint.** Invariant: a fact about a peer is read from its own
   declaration on the connection being served, never recomputed — but a
   DECLARATION is not yet an ADDRESS: handle derivation, self-file keys
   and on-disk filenames keep main's exact rules until that migration is
   its own later decision (manager review: Codex found nine blockers
   turning on exactly this conflation).
   - One resolver, `sot_log::host_name()` (and its shell mirror,
     `sot_host`): `SOT_SELF_HOST` if set and NON-EMPTY (empty is unset,
     falling through, in both), else the first label of
     `gethostname`/`hostname -s`, trimmed then lowercased identically in
     both; an empty RESOLVED hostname (never an empty override) is the
     startup error. A NEW variable — `SOT_HOST` already means the SSH
     target a remote frontend dials (`launch-sot.ps1`/`.sh`); reusing it
     silently renamed a frontend to wherever it dials (Codex-reproduced).
     Feeds hello, logs and display ONLY — `state_host()`, comm's handle
     derivation, the frontend's own address (below), its persisted-state
     filename, and the monitor's local-vs-SSH routing keep their own
     resolver untouched: a display override changing ANY of those is the
     bug this decision stops. Not pinned into a spawned pane/capsule's
     env — inheritance already carries an override the daemon has.
   - `HelloReq` gains `host`, `role ∈ {fe, bridge, cli, agent}`,
     `instance`, and `name` (a NON-frontend's own declared handle).
     `fe_handle` is untouched — no rename, no alias; unifying it with
     `name` is sprint 2's `PROTOCOL_VERSION` bump. `role` gates
     active-frontend selection/audience as an EXTRA check on `fe_handle`
     (present and not `"fe"` excludes; absent falls back to `fe_handle`
     alone, as before this lane). `HelloRes` is unchanged. The log line
     and `version.query`'s roster print the declaration alongside
     `fe_handle`; six pasted comm hello lines become one
     `sot_hello_frame`.
   - The frontend constructs one `FrontendIdentity {host, instance, name,
     role}` at process start (`instance` from `SOT_FE_INSTANCE`, else one
     mint per process, replacing a bare function that re-sampled the
     clock per call); shared by every connection, reconnect and input
     attribution. `name` (this frontend's ADDRESS) is NOT derived from
     `host`: main's exact pre-lane `win-fe-<host>` off
     `$HOSTNAME`/`$COMPUTERNAME`, unconditionally — an early draft built
     it from the declared host and broke every explicit `--fe` target
     (Codex-reproduced, round 2).
   - The frontend keys each connection by its dial (the hosts.toml
     section) — NEVER re-homed to the declaration (tried and rejected,
     below). One map records what each dial declared, read by ONE
     display projection (`host_label`: the declaration if known, else the
     dial key) used everywhere a host is named — Hosts mode, Sessions,
     the status line (including on a mere host SWITCH, not only a fresh
     `Connected`), connect/disconnect logs — no separate truncation. It
     feeds no refusal: closing a duplicate declaration needs a real
     transport-shutdown path, which does not exist, so `hosts.rs`'s
     static same-port skip stays the one thing preventing two dials from
     reaching the same daemon.
   - **`agent.join {workspace_id, handle}`**, one op: `comm-join.sh`
     declares the session's handle through the EXISTING
     `sot_daemon_endpoint` resolution (`SOT_SOCKET` first on Unix, its
     pre-existing bare-path meaning; the local pipe on Windows) —
     `comm-lib.sh`'s `sot_host` shares the identity rule above, JSON
     -escaped on the wire; comm's own handle derivation (clamp,
     digest-suffix) is untouched. The daemon applies the mutation under
     the SAME per-row lifecycle guard `workspace.destroy` holds (ADR 0043
     decision 33) — a guarded IN-PLACE update, re-checked once the guard
     is actually held, never a replacement and never a resurrection of a
     row a concurrent destroy just removed — and answers `ok` only once
     durably persisted; a save failure is reported, not swallowed. The
     self-file read-back (`capsule_comm_handle`) stays as the FALLBACK
     for an undeclared row, deleted only with family H.
   Net across the branch: about 1975 lines added, 350 removed. Wire:
   additive.

2. **The daemon that spawned a row is the owner of record for its facts.**
   Invariant: one authority per fact, observed or declared to the row's
   daemon, persisted and served by it; every other derivation is deleted.
   - **Phase — one observer per row, one writer, reachability is not an
     ending.** `phase` on `Workspace`, never persisted. Its one writer is
     the row's lifecycle observer: a persistent supervisor-lane client
     (PREREQUISITE — `query_status`, `supervisor_client.rs:146`, opens a
     connection per call) polling `status` at the attach client's 2 s
     liveness interval (`fe_client_io.rs:166`). When a resident worker
     (decision 3) is attached, ITS liveness poll on the SAME connection is
     the observation, failures included — a supervisor-lane connection per
     row; the observer polls on its own for rows without a resident. Settle
     and watchdog observations are fed INTO the observer with an ownership
     check (the observation's spawn generation must be the row's current
     one) and never write `phase` directly. `unreachable` needs two
     consecutive failed, deadline-bounded rounds (success resets the
     count); `ended_no_respawn`/`terminal` LATCH — no later observation in
     the same generation clears one, only a new spawn generation does. Lane
     events are not available (the supervisor wire has no lifecycle
     subscription, `wire.rs:665`) and are not pursued.
   - **Selection is the one start intent.** `pty.open` on a capsule row
     answers `attach_direct` at once from the phase cell and, when the
     phase is not `ready`, runs the existing guarded activation helper
     (`ensure_started`, `capsule_workspace.rs:1647`: first start, resume,
     the stop → resume → reset retirement, the inert-anchor protection —
     unchanged) asynchronously, never awaited by the request. A failed
     activation's error is kept (in memory, like `phase`) on
     `workspace.list` until the next attempt — never implying `Terminal`.
     Only the blocking `phase_of` preflight (`server.rs:1961-2064`) is
     deleted. The raw-lane bridge's dial (`lane_bridge.rs:119`) uses that
     same helper. `workspace.list` is pure memory. The observer and the
     resident never start work; a passive resync never restarts ended work.
     This is a smaller change than the bridge rewrite round 1 proposed.
   - **Record location.** `record_dir` is persisted, written at create for
     new rows from the qualified state root of that moment. Existing rows
     migrate LAZILY: when the daemon next finds the row's supervisor lane
     or pointer under the root in force — evidence of ownership — it
     writes `record_dir`; a row without such evidence stays unmigrated and
     inert, never guessed. Once written, the thirteen
     `state_dir_for(sot_state_dir(), id)` reconstructions read the row.
   - **Latest result — what the pending map does today, owned by the
     daemon.** `Workspace` gains `result {path, caption, rev}`, one
     persisted monotonic `rev` per row. `fe.command.send preview|reveal`
     increments, persists and publishes, serialized per row, before
     acknowledging; a persistence failure is reported to the publisher;
     the command event carries `{workspace_id, rev}` so a viewing frontend
     badges or clears without a list refresh; the legacy relay publication
     (`sot-nav.sh:73` through `agent.send`) moves onto `fe.command.send`.
     A frontend persists `seen_rev` per row and sets it to `rev` when the
     user switches to that row or when a `preview.get` for that result
     path completes while the row is active; no revision rides the render
     path. A caption accompanies only its own path (ADR 0025's per-file
     retention is replaced); `roi` stays a one-shot aim.
   - **Agent work state stays as today** (the registry read); the reducer
     port is deferred.
   About 200 lines added in the daemon, 300 removed. Wire: list-entry
   fields, `agent.join`, `{workspace_id, rev}` on the command event.

3. **Resident attach: the daemon relays the voyage's own frames.**
   Invariant: the checkpoint is the one resync mechanism (ADR 0045
   decision 1's rejection stands for *rendering*, re-scoped to allow
   *relaying*); the terminal's first paint after a switch waits for no
   network operation.
   - **Transport worker (PREREQUISITE).** The attach client's transport
     half — connect, hello, attach, checkpoint reassembly, reader, take,
     input transactions and quit (unchanged), reconnect — is extracted from
     `FeAttachClient` (`fe_client_io.rs:863` owns a parser) into a worker
     with an event sink and bounded ingress (`:1092` is unbounded today).
     The daemon runs it per ready row with no parser; the frontend's
     rendering client wraps it and restores checkpoints with
     `Parser::restore_screen` (`vt100/src/parser.rs:128`) directly.
   - **Framing.** `capsule.evt {workspace_id, kind, seq, last?, len}` +
     the control protocol's binary blob tail (`codec.rs:48`), bounded at
     the lane's chunk size; a closed vocabulary: `reset`, `checkpoint`,
     `output`, `pen`, `geometry`, `dropped`. No base64 (a legal chunk,
     `wire.rs:296`, exceeds the 1 MiB envelope cap, `codec.rs:18`).
   - **Attach proto v3 is REQUIRED for resident service.** The capsule
     emits to every watcher, in order, behind any still-transferring
     checkpoint (drained with the watcher's output after the final chunk,
     never lost to the checkpoint's own dimensions — raw v3 client and
     relay alike): a pen snapshot on attach (`holder: Option<..>`, always
     first), `pen_changed` on take (`attach_proto.rs:1188`) and on
     capability loss at disconnect (`:1746`), `geometry` after a resize
     (`:1223`). Checkpoint format stays v2; durable command bytes
     unchanged. v1/v2 stay for raw clients; there is no v2 degraded mode.
     Legs on an older proto are cycled by the release that ships v3 — ADR
     0045 decision 10's mechanism, "entered with every capsule row ended" —
     and the daemon reports such rows as `attach_unsupported` until cycled.
   - **Resync is a serialized per-row cutover** — a new frontend hello, the
     worker's reattachment, or an explicit `capsule.subscribe` triggers it,
     never a laggard (see backpressure) — over a NEW watcher connection (a
     second `Attach` on a watcher closes it, `attach_proto.rs:1340`):
     complete outstanding input transactions on the old worker within an
     absolute resident-command deadline (`InputRefusedStale` is an
     intermediate refusal the worker retakes on, not completion; only
     `InputRecorded`/`InputUnknown` or the deadline ends the wait — on
     expiry, unsubmitted work fails at once; submitted work retires the old
     connection and outstanding tuple FIRST, reports unknown, no retry
     running; never mint a fresh idempotency key for an uncertain completion
     — `fe_client.rs:496`'s reconnect rules are the contract), stop
     publishing the old attachment, emit `reset`, deliver the complete new
     checkpoint, then only its deltas, close the old attachment. A
     subscriber discards incomplete checkpoint data and applies no deltas
     while unsynchronized — this restores screen state and retained history,
     not every frame from the gap. Admission is authoritative: a refused or
     failed attach is a visible state, never a count preflight (the watcher
     count is private capsule state).
   - **A live stream survives supervisor unreachability** (the client's
     own contract, `fe_client_io.rs:2512`: the attach connection is
     demonstrably alive); new attachments start only on validated
     readiness; the resident stops only on `ended_no_respawn`/`terminal`.
   - **Backpressure: exhaustion is terminal for that subscriber.** Byte budgets
     (per-row publish, per-frontend queue), not message counts. A subscriber
     over budget waits, bounded, for the next independently scheduled cutover;
     if none, it receives `dropped {reason}` (or the connection closes with a
     reason) and gets no further frames for that row until it sends the
     explicit `capsule.subscribe` — the user reselecting the row (like a hello;
     other healthy subscribers may resync too). Healthy subscribers are never
     reset for a laggard; no automatic re-subscribe runs.
   - **Input.** One serialized command queue per row; requests are
     enqueued at once and complete through the existing off-loop reply
     path (`server.rs:1359`, never awaited inline as `:2289` does today);
     controller changes only between completed transactions; attribution
     is the accepted hello's `name/instance`. Rule for several frontends:
     **last taker wins, the demoted frontend is told** (`pen`).
     `sot-fe type` goes through the queue; a raw-lane client contends at
     the voyage as today. `pty.resize` gains `workspace_id`; only the
     driving frontend resizes.
   - **Frontend.** One parser per resident row; a switch selects the
     parser; `workspace.activate` and nav loads still run but first paint
     waits on none. Support is read from the hello's declared capability,
     never inferred from a missing event; pending, refused and failed
     attachment are distinct states. The per-switch dial, the teardown
     and rebuild, the frozen snapshot and the per-attach controller id are
     deleted; `lane.connect` stays for headless and raw-lane clients.
   About 450 lines added, 450 removed. Wire: one evt, one attach-proto
   version.

4. **One launch recipe, the daemon's `agent_argv`, exec'd through.**
   Invariant: an agent's launch policy has one owner. `sotd agent-exec
   <kind> [flags…]` (a pure subcommand beside `session-socket-path`,
   `main.rs:168`) resolves the binary (`resolve_claude`,
   `capsule_workspace.rs:294`), scrubs the nesting env, prepends
   `~/.local/bin`, appends the bootstrap skill after the flags it is
   given, and execs — Unix only, never in a producer path. `ccb`/`ccbe`
   become `exec sotd agent-exec claude "$@"` (a bare `ccb` stays fresh;
   `--continue` is the daemon's default for capsule rows, not
   `agent-exec`'s). Limitation, stated: `producer_argv` stays captured at
   create, so a recipe change reaches a row when its supervisor is next
   spawned — the mechanism that already carried `--continue` to respawned
   rows. Deletes two of three copies of the scrub list, PATH rule and
   flags (~60 lines of shell). Windows `agent_argv` returns a bare name
   (`:251`): out of scope; no Codex launch support is implied. Amends ADR
   0023, 0042 decision 1.

5. **One runtime, now-part only.** Invariant (ADR 0042): nothing new runs
   on tmux. `workspace.create` refuses `"tmux"` wherever the capsule
   runtime compiles (`handlers.rs:4478`), answering
   `runtime_not_available`. `install.sh` requires tmux when an existing
   row needs it *or* the host lacks capsule support (macOS,
   `capsule_workspace.rs:277`), reading rows with the same host and
   config-dir rules as workspace loading (`install.sh:32`); a fresh
   capsule-capable install needs no tmux. The tmux deletion, family F and
   the `PROTOCOL_VERSION` bump are one second-sprint change.

## Consequences

- **Wire, additive.** `HelloReq {host, role, instance, name}` alongside
  the unrenamed `fe_handle`; `WorkspaceListEntry {agent_handle, result?}`
  beside `phase`; `agent.join`; `capsule.evt` with blob tails;
  `{workspace_id, rev}` on the command event; `pty.resize.workspace_id`;
  attach proto v3. `PROTOCOL_VERSION` stays 1; active selection stays on
  `fe_handle`, untouched, so a legacy frontend needs no converge step for
  this decision.
- **Persistence.** The row toml gains `record_dir`, `agent_handle`,
  `result_path`, `result_caption`, `result_rev`; each defaults when
  absent. `phase` is never persisted. No directory is renamed this sprint.
- **Memory and bandwidth.** Frontend: one parser per resident row (13–32
  MB at 5000 rows of scrollback; +80 to +250 MB at six to eight rows
  against 363 MB today); the scrollback knob bounds parsers, the byte
  budgets bound queues. Daemon: one worker and one supervisor-lane
  connection per ready row, no screen. Concurrent typing is visible, not
  lossless (ADR 0042); a declaration is attribution, not authentication
  (ADR 0042 decision 6).

## Deferred to the second sprint

- **The work-state reducer port** (`agent.status`, the heartbeat writer,
  the registry projection) lands with family H: two authorities cannot be
  made one before the registry's other writers
  (`comm-status-heartbeat.sh`) and readers move with delivery.
- **The recipe file**, its publication lifecycle and the supervisor-side
  read: the preferred shape if adoption at spawn is ever needed; respawn
  adoption is enough today. Windows absolute resolution for `agent_argv`
  and Codex launch support are likewise out of scope.
- C (endpoint file, connection plan), F (one row name), G (token,
  admission), H (delivery through the local daemon; deletes the self-file
  pin and the registry prune), the tmux deletion.

## Rejected alternatives

Frontend-resident N rows (option B: warm rows only, N reconnecting workers in
the component with the least authority); a leaner dial (option C: ~250–300 ms
remote, below neither target); `sotd agent-exec` in the producer path (a
wrapper mapping `sotd.exe` for the agent's lifetime); same-connection
re-attach or daemon-held screens for resync (refused by the capsule; a second
resync mechanism); a v2 degraded mode for the resident (pen and geometry are
exactly what v2 lacks); base64 `capsule.evt` (exceeds the envelope cap on a
legal chunk); rekeying the frontend's host-keyed state to the declared host:
it forces a rebinding of every host-keyed structure on hello for no invariant
the label does not serve (measured in lane A: roughly 640 lines).

## Lanes

| Lane | Scope | Depends on |
|------|-------|------------|
| A | `host_name()`/`SOT_SELF_HOST`, `FrontendIdentity`, the hello declaration, the roster/log, `sot_hello_frame`/`sot_host`, `agent.join` (guarded update, existing `SOT_SOCKET`). **Gate:** display-only `host_label`, dial-keyed, never a rekey | — |
| B1 | The observer (shared lane connection), `phase` from memory, the async activation on `pty.open`, the blocking preflight deleted | persistent supervisor client |
| B2a | `record_dir` at create, lazy migration by evidence, the reconstructions deleted | A |
| B2c | `result {path, caption, rev}`, one publication path, `sot-nav.sh` migrated, `seen_rev` | B2a |
| B3a | Transport-worker extraction, bounded ingress, `capsule.evt` framing and vocabulary | — |
| B3b1 | Attach proto v3 owner events; `attach_unsupported` | — |
| B3b2 | Relay: cutover with input completion, budgets, terminal exhaustion, `dropped` | B1, B3a, B3b1 |
| B3b3 | Input/control wiring: the command queue, off-loop completion, `pty.resize` routing | B3b2 |
| B3c | Frontend: per-row parsers, local switch, the dial deleted, capability from hello | A, B3b3 |
| D | `sotd agent-exec`; `ccb`/`ccbe` one line | — |
| E | `"tmux"` refused on capsule-capable hosts; the conditional install prerequisite | — |

Order: A ∥ B1 ∥ B3a ∥ B3b1 ∥ D ∥ E → B2a → B2c; B3b1 → B3b2 → B3b3 →
B3c. Each lane: brief, implementation, manager review, CI, a live proof
on the backend host, merge; Codex on the design-bearing ones.
