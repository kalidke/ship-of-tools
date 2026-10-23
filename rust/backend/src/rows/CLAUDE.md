# rust/backend/src/rows: rows (charter)

## Idea
A row is one project folder on this computer running one agent program (Claude Code, Codex) or none, in a terminal its
supervisor keeps (the capsule). The daemon registers the row under an id that survives restarts, starts and ends its
supervisor through one guard, keeps its phase, binds the comm handle its session declares, and shows its work-state:
what the row is doing, and whether it waits on its person.

## Contract (pass 3)

Guarantees, each with the mechanism it rests on.

Identity:
- A row's id survives every daemon restart and every refresh of its label. The id is minted once, at the first
  `workspace.create` of a slug (`Workspace::from_label`), written in the row's toml (`store::save`), loaded at boot
  (`store::scan_disk`), and `Workspaces::insert` keeps the id of a slug already registered. The toml is a projection of
  the registry: only `store::save` writes it, and a row changes through an op, never through its file.
- A row's comm handle has one answer, and no two rows hold one handle. The answer is `bound_handle`: the
  handle the row's session declared (`agent.join`), else the name the row was created with, unless another row declares
  that name. A join moves a handle: `set_agent_handle` clears it from every other row under the registry's write lock;
  at boot `store::scan_disk` keeps a handle several tomls declare only on the row the comm registry names as its last
  joiner (`clear_shared_handles`). This holds by construction: the declared cell (`handle::Declared`) is private to
  `handle.rs`, so the binding is read only through `bound_handle` (the list, the work-state, the liveness stamp, a
  join from outside any row) and the declared value only through `declared_handle` (the row's toml, the list's
  `agent_handle` field, and the wake, since a session that never joined has no mail to wake for); it changes only
  through `set_agent_handle`, `clear_shared_handles` and `clear_declared`. The stored name (`agent_name`) is the handle a
  row was created with, written into its pinned self file at its start when that file names nothing yet
  (`comm::identity::self_file::pin_row`; no `SOT_COMM_NAME` pin, decision 0031), and the prune's key; it binds a handle only through `bound_handle`.
- The default row is an inert anchor when its agent is `none`, on every runtime (`anchor::is_inert_default_anchor`, the
  one predicate); its declared handle survives the boot re-seed (`seed_default_row`).

Supervisor:
- One guard per row. Every change to a row's supervisor (create, start on attach, the boot resume, the watchdog's
  restart, reauth, destroy) and a join's save holds the row's one lifecycle guard for its whole duration and rechecks
  the row once it holds it (`Workspaces::capsule_guard`, minted only for a registered row, under the registry's write
  lock, and dropped with the row by `remove_by_id`).
- No second supervisor over a live one. Every start path reaches one launch (`spawn::detach::spawn_detached_supervisor`),
  which claims the row's authority fence (`supervisor.lock`) before it forks; a claimed fence forks nothing, and the
  start is told the authority is pending (`run::admission`), never success. On Unix the durable parent holds the claim
  (`spawn/durable/`), so neither the daemon's death nor a second birth takes an accepted birth's fence.
- The watchdog's restart budget is the one restart budget for a row's agent: the supervisor never starts the agent a
  second time on its own. An agent that cannot start is a count, not a rate: after five consecutive restarts of
  supervisors that never reached `ready` (the row's phase never read `ready` at least two seconds after the cell adopted
  that supervisor; one that died before any observation named it, or flashed `ready` and was gone, never did) the next exit marks the row `terminal`,
  however long the starts and settles take, and the count resets when one reaches `ready`; restarts of supervisors that
  did reach `ready` keep five within 60 s (`RESTART_BACKOFFS` 1, 3, 7, 15, 30 s either way). The watchdog reads the
  supervisor's exit: 0 ends it, 69 marks the row `terminal`, 70 leaves the row to the next attach, and every other end
  restarts it with `--resume` within the budget: 75 (the agent could not be spawned), 76 (it started and ended by
  itself) and a killed supervisor (no code), which takes its agent with it. 75 and 76 are one path for the restart and
  differ only in the log; a restart spawns the row's own recipe, never a reauth's `--resume`. Limits: readiness is a heuristic (the row's phase read `ready` two seconds after adoption; a delayed observation, or a
  supervisor that died before settling and was never named, reads never-ready, and a flash read late can read ready), and
  the watchdog's identity for a supervisor comes only from its settle and is not refreshed by a later observation or
  cleared by a failed spawn (queue item RUN-IDENTITY-READY, pass 6c-1: each run gets an identity on the row socket);
  a replacement's exit 69 before it answered is latched only by a give-up, not by `TerminalUnclaimed`, which a claimed
  cell refuses. The give-up latches
  `terminal` under the row's guard, whichever supervisor the cell was last claimed by. Limit, accepted for pass 6b and
  closed by pass 6c-1 with a guarantee and its red test: a supervisor found alive at boot has no watchdog (`resume_all`
  adopts it and never watches it), so after its agent ends by itself the row stays `unreachable` until it is selected,
  whose attach restarts it.
- No start begins once the close has begun: `Workspaces::begin_start` refuses after `close_gate_and_settle`, which waits
  for the starts already admitted (`gate.rs`).
- A row's phase follows its current supervisor. The phase cell changes only through
  `Workspace::apply_phase_observation`, which rejects an observation from a supervisor other than the cell's epoch and
  latches `terminal` (for its supervisor) and `ended_no_respawn` (for its run: a supervisor's runs order by their ids, a
  resting supervisor reports the run it rests on, a lower or unnamed run is a stale observation and a strictly newer run
  releases the latch; a supervisor restarted onto a requested end names no run, and the latch on no run is released by
  its first run: the capsule's `ended` record carries no run, by decision, so the cell never compares runs across
  supervisors). Each row's one observer task holds the row's id and feeds the row registered under it at each
  round (`run::observer`), so a refreshed row's phase follows its new supervisor.
- A state folder had a supervisor iff it holds the fence file `supervisor.lock`: on Unix the daemon claims it before it
  forks and a supervisor takes it before anything else, so such a folder whose row socket does not answer reads
  `unreachable`, not `stopped`, including a start whose fork failed after the claim, and the boot resume and the
  watchdog's recheck act on it. A folder without the file reads `stopped` (never started). Nothing that only observes a
  row creates the file: `end_run` under the row's guard (every caller holds it, and no start begins without it) reads a
  missing file as never started and takes nothing; only the state-folder sweep and retirement, which work on folders no
  registered row names (the boot resume reads registered rows only), take the fence with the creating API. Limit
  (Windows): the supervisor is spawned before it takes its fence (`spawn/detach.rs`), so one that exits in that window
  leaves no marker for the recheck or the boot resume.

The daemon's calls on a row's supervisor are ops of the row socket, through `sot_log::attach_client::supervisor_client`
(`query_status`, `end_run`, `reset`, `stop`, and the observer's `Persistent`), each bounded; a row of another row number
is `foreign` and one that answers no such op, or none, is `unreachable`. An outcome the call could not settle is
settled by reading the same supervisor's status, never by asking again: a lost end whose run is gone is `Ended`, a
lost reset after which status names a run other than the one the supervisor rested on (possibly none) is `Ok`, one
still resting or terminal on that run is an error, and what stays unsettled is `NotEnded` (an `end_run`) or an error (a
`reset`), so the row is kept and lifecycle's or the caller's retry reads the true state. Management shares
the row's 8 connections with the viewers; a dial that gets no slot is one failed observation, never an end.

Ends:
- A row is reported ended only after a confirmed end or the destroy proof: the supervisor fence can be taken (or no
  fence file exists, which proves never started) and nothing answers on the row's socket (`run::end_run`); an already
  resting supervisor is stopped and its scopes ended without the proof, and a stop that fails is logged, not an error.
  Then its comm entries are pruned, its toml removed durably, the row unregistered and
  `workspace.changed` sent. `workspace.destroy` renames a non-default row's state folder to `.closed-<id>` and removes
  it on a thread; a row the end API ends keeps its folder for a later start's sweep; the default row ends its run and
  stays registered.
- Lifecycle ends rows only through the end API (`end.rs`): `end_every` ends the rows it is given and the drawer
  concurrently, at most `LANE_CONCURRENCY` at a time, tries a refused end again once a second until the deadline it is
  given, and reports each row ended, not ended, or to forget; `forget` drops the rows a past end could not remove. With
  `run::resume::resume_all` and `Workspaces::close_gate_and_settle` that is all lifecycle calls.
- The start sweep removes every tombstone, and removes a state folder no registry names only on a whole load of every
  registry, when nothing runs in it, it holds no `row-scopes` record and nothing in it was written for 24 hours; a link
  at or inside the `workspaces` folder is never followed (`state_folder.rs`).

Work-state. A row with a joined agent has one work-state, which the window shows as the color of its name:

| state | color | means | summary |
|---|---|---|---|
| blocked | red | the row waits on its person: a question it asked, or a dialog on its screen | the question or the dialog |
| working | green | a turn is running | the last declaration line |
| waiting | purple | the row waits on a machine (a job, a peer) and needs nothing from its person | the wait |
| done | blue | a turn its person started ended, and they have not looked at the row since | the last line |
| idle | gray | at rest | the last line |

- The state is a reduction of facts, recomputed at every write in one place (`work_state::reduce`). The facts are on the
  row's entry in the comm registry (`.agents[<handle>]`): `floor` (a turn runs, started by `user` or `machine`),
  `question`, `waiting`, `done`, `dialog` and `note`. Precedence: a dialog, then a question while no turn runs, then a
  floor, a wait, done, idle. `blocked` keeps a wait; `waiting` clears a question; `working`, `idle` and `done` clear
  both; a question or a wait without text of its own is refused (exit 2), so no red or purple row is ever textless.
- One writer path. Every write of a work-state fact is `work_state::apply_event` (through `write_event`), one read-decide-write under the registry lock
  on a fresh read (messaging's `edit_entry`). A session reaches it through `sotd work-state`, which `comm-status.sh`
  runs; the daemon calls it in its own process.
- Who may set it. The row's own session: its hooks' events (`prompt` with its origin, `stop`, `join`, a Codex `dialog`,
  `resumed`, the heartbeat's `tick`) and its declarations (`comm-status.sh working|idle|blocked|waiting|done ["text"]`,
  and the Stop hook's closing markers on its behalf). A second agent inside the session stamps nothing
  (`sot_require_agent`), and a process outside the row resolves no handle of the row's. The daemon sets two facts only:
  the dialog it reads on a Claude row's screen, and a person's read (`workspace.activate` with `read: true`, which the
  window sends after ten seconds on the row), which clears `done` without restamping the row's time.
- One reader path. `workspace.list` carries each row's `agent_state`, `agent_summary` and `agent_status_at`, read fresh
  from the registry through the row's bound handle and only from an entry of this host (`work_state::shown_of`); the
  work-state poll compares the same answer every 1.5 s and publishes `workspace.changed` (action `agent_state`) when what
  any row shows changed (`work_state::poll`).
- How the window shows it: the row's name in the Sessions tree and the strip takes the state's color (Julia red, green,
  purple and blue, and gray), flashes at a change, and wilts when it is `working` and its time is ten minutes old; a busy
  turn's heartbeat restamps the time at most once a minute. The summary follows the name.
- A row waiting on its person reads red. A Ready Claude row whose screen shows a dialog that waits on a person (a
  permission prompt, a select menu) reads red with the dialog's question as its summary within one dialog tick
  (`DIALOG_EVERY`, 5 s, plus the sweep), whatever its turn state and on every OS, and within one tick of the dialog
  closing reads what its other facts say. The tick reads every Ready Claude row with a bound handle but one already red
  by its question, one watcher attach each, which takes no pen, resizes nothing and writes no record
  (`work_state::dialog`). A Codex row's dialog is its own `PermissionRequest` hook's, cleared by the call's return, the
  turn's end or the next prompt.
- Limits: a dialog that closes between the tick's read and its write reads red for one more tick; a turn that ends with
  no Stop (Esc, a bare No, a reauth, an API error) leaves the row `working` until its next turn.

Owns: the registry (`Workspaces`) and the row (`Workspace`) with its phase cell, guards, run gate and the default row's
rule; on disk `<config>/workspaces-<host>/<slug>.toml`, `<state>/workspaces/<id>/` with its tombstones, and
`workspaces/.registries-of`; the ops `workspace.create`, `workspace.destroy`, `workspace.list`, `workspace.activate`,
`workspace.reauth`, `agent.join`, `pty.open`, `pty.input`, `pty.screen` and `lane.connect` (which pipes the row's one socket, `row-<h>`, after resuming an absent one), and the `workspace.changed`
event; the supervisor's launch (state root, detached spawn per OS, durable parent, the Linux row scope), its start,
watchdog, observer, end and boot resume (`spawn/`, `run/`); the daemon's headless attach client (`run/headless.rs`, a watcher over the row socket that takes the pen once per run, only
to type; after a reconnect to the same run it does not take it again, and in a replacement run its queued input is answered `run ended`), which the wake uses too; the liveness stamp's choice of handles (the rows that run); work-state: the reduction and
`sotd work-state`, the dialog tick and the poll (`work_state/`), and the scripts in `comm/work_state/`
(`comm-status.sh`, `comm-turn-auditor.sh`, the status hooks), with the reference page
`comm/adapters/claude/sot-comm/references/work-state.md`. Housed there and messaging's: the Stop hook's mail gate and
`comm-subagent-guard.sh`, until pass 4 moves the hooks to `sotd comm`.

Calls in: the window and the row CLIs (`agents/spawn/`, `sot-fe`) through the ops above; lifecycle (`end_every`,
`forget`, `resume_all`, `close_gate_and_settle`, `claim_create`, `list`); the server (`row_or_reply`, `resolve`,
`capsule_guard`, `seed_default_row`, the bus setters); files and sidecars (a row's lazy handles); messaging's wake
(the declared handle, the headless client, `state_dir_for`, the probe); every session's hooks and declarations
(`comm-status.sh`, which runs `sotd work-state`). The interfaces kept: every op's payload, `comm-status.sh`'s verbs and
exit codes, and the registry entry's fields. Gone: `SOT_COMM_ROW_HANDLE`, the daemon's own entry into `comm-status.sh`,
which a newer script refuses (exit 2): the daemon writes in its own process, and an older daemon running beside newer
scripts during an install logs one refused dialog stamp a row until it restarts.

Calls out: capsule (`sot-capsule supervise`, the supervisor client, the attach client); agents (argv, environment,
folder trust); sidecars (`Kernel`, `Repl`); files (`FilesMode`, `ConceptStore`, `Watcher`); messaging (`edit_entry`,
`read_comm_agents`, `remove_comm_agents_for_workspace`, `stamp_last_seen`, `last_joiner`, the wake's `dialog_of`);
platform (dirs, the declared host, durable writes, locks); containment (`Signal`).

Does not: run the agent's terminal or keep its record (capsule); decide when rows end (lifecycle asks); read a screen for
mail or type into a prompt (messaging's wake); build an agent's argv (agents); write the registry's address-book fields
or deliver mail (messaging); judge a turn's text (the turn auditor's Haiku call nudges the session; only the session's
own stamp changes a fact).

Crate: not drawn in this pass. Rows' target dependencies are capsule, agents, sidecars, files, messaging, containment
and platform; only capsule and platform (inside `sot-log`) are crates. A `sot-rows` crate needs agents, sidecars, files,
messaging and containment as crates first, and the server's reply type (`HandlerOutput`) moved to wire, since every op
handler here returns it.

Platforms: Linux runs the durable parent and a systemd scope per row (`spawn/row_scope.rs`), the one way a destroy ends
a child that left the agent's process group; macOS runs the durable parent and no scope, so only controlled ends end a
row's children; Windows creates the supervisor directly, breaking away from the daemon's job, and reads a held fence as
the supervisor's own exit 70. `sotd work-state` and the dialog tick run on all three. The scripts run under bash 3.2 and
Git Bash (messaging's rule, comm/CLAUDE.md).

Tests: one unit test per guarantee, `cargo test -p sot-backend rows::` (each named in the pass's result); the row half
of the integration suite `capsule_workspaces` on real daemons and capsules; `comm/tests/test-status-floor.sh` for the
hooks' events reaching the reduction.

Unbuilt: ROW-RESTART (queue item 1, a feature): one restart action for a row in phase `terminal` or `ended_no_respawn`
(`workspace.restart`, `sot-fe restart <ws> [--resume <id>]`, Shift+R in Sessions), keeping the row's id and handle,
with a fresh leg and a resume by id on request. No guarantee here needs it: an attach resets an ended row, and a reauth
restarts a live one in place.

## Folders
- `ops/`: the row ops clients call.
- `reauth/`: `workspace.reauth`, the accept half and the restart runner.
- `run/`: a row's run as the daemon sees it: phase words, observer, headless client, start, end, activation, watchdog,
  boot resume.
- `spawn/`: launching a row's `sot-capsule supervise`: state-root checks, the detached spawn per OS, the durable parent,
  the Linux row scope.
- `store/`: the row toml store, its codec and the boot migrations.
- `work_state/`: the reduction, `sotd work-state`, the dialog tick and the poll.

## Files
- `mod.rs`: the `Workspace`, `Workspaces` and `Inner` structs, `row_or_reply`, the `WorkspaceChanged` event and the
  session name rule.
- `workspace.rs`: the row's methods, `Phase`, `Observation`, `SupervisorIdentity`, the phase cell, `now_unix`.
- `registry.rs`: `Workspaces` insert, lookup, removal, observers, buses, the account cell and the per-row guard.
- `handle.rs`: the declared cell (`Declared`), `bound_handle`, `declared_handle`, the two moves of a handle
  (`set_agent_handle`, `clear_shared_handles`) and the liveness stamp's choice of handles.
- `end.rs`: the end API lifecycle calls: `end_every`, `forget`, `EndReport`.
- `gate.rs`: `RunGate`, `StartPermit`, `begin_start`, `close_gate_and_settle`.
- `anchor.rs`: the inert default anchor rule, `reset_agent_to_none`, `end_default_row_run`, `default_row_launch_seed`,
  `seed_default_row`.
- `state_folder.rs`: `close_row_folder`, the start sweep (`sweep_with`, `sweep_state_folders`), `retire`,
  `is_row_folder_name`, `AGE_FLOOR`.

## Start here
`mod.rs` for the structs, then `registry.rs::insert` for how a row enters. A phase change:
`workspace.rs::apply_phase_observation`. A row's handle: `handle.rs`. A work-state write: `work_state/mod.rs::apply_event`.
