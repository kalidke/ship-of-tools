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
  supervisors that never reached `ready` (the supervisor's own word: its watch told `ready`, which it does once the agent
  has lived 2 s, before it ended; one that died before any watch named it, or whose agent ended sooner, never did) the
  next exit marks the row `terminal`, however long the starts and settles take, and the count resets when one reaches
  `ready`; restarts of supervisors that did reach `ready` keep five within 60 s (`RESTART_BACKOFFS` 1, 3, 7, 15, 30 s
  either way). The watchdog reads the supervisor's exit: 0 ends it, 69 marks the row `terminal`, 70 leaves the row to
  the next attach, and every other end restarts it with `--resume` within the budget: 75 (the agent could not be
  spawned), 76 (it started and ended by itself) and a killed supervisor (no code). The agent dies with a killed
  supervisor within capsule guarantee 2's limits: on macOS an agent that ignores SIGHUP can outlive it. 75 and 76
  are one path for the restart and differ only in the log; a restart spawns the row's own recipe, never a reauth's
  `--resume`. The watch record (`Workspace::watched`: the supervisor the row's watch last named and whether it told
  `ready`) is written by the observer, kept when a watch reconnects to the same supervisor, and read by the watchdog at
  an exit. Every outcome is bound to the spawn it came from: the row counts the spawns this daemon makes
  (`Workspace::begin_spawn`, under the row's guard just before each spawn, which also clears the record), and an
  observer's round and a watchdog each carry the generation they began under. A watch's first reply from a round that
  began before a spawn is refused (it may be the supervisor the spawn replaces); an exit is acted on, under the guard,
  only while the row is still at its generation, so an old supervisor's 69 never latches its replacement and an exit a
  spawn's watchdog already handled is never handed to a second one. A 69 marks the supervisor the record names
  `terminal` on its own word; a 69 with no record (the supervisor exited before it answered, as every bootstrap failure
  does) and the give-up mark the row `terminal` under the row's guard, rechecked once (`may_mark_terminal`: registered,
  not already `terminal`, and nothing answers: `unreachable`, or `stopped`, which only Windows can read of a first
  start whose supervisor has not taken its fence). Limit: a spawn refused as contended also begins a generation, so the
  live supervisor it found loses its record and its exit is left to the redial.
- A supervisor this daemon did not launch (found alive at boot) is watched all the same, and its exit is restarted
  within the same budget with no selection. At a watch's end the observer waits for the supervisor's process to end
  (`Watch::end`, at most 5 s), so a decision is not taken while the exiting supervisor still answers or holds its fence;
  if the process ended, the cell reads `unreachable` (a rest does not outlive its supervisor; `terminal` does), and if
  nothing owns the row, no spawn has begun since, and the watch did not carry exit 0 (a stop, which nothing restarts),
  it takes the row's guard, checks again, and installs a watchdog whose first outcome is the exit the watch carried
  (none for a kill), exactly as a child's. The watchdog announces its ownership before the install returns, under that
  guard. Limits: a supervisor whose death is not confirmed within `Watch::end`'s 5 s (a slow teardown, a listener lost
  for good) is left to the redial, whose failed dials establish no exit, so it reads `unreachable` or keeps its rest
  until it is selected; a row whose recipe cannot be built is logged and left as such.
- A row a 0.6 release left is retired before it is started or ended (`spawn/retire_06.rs`, deleted in the release after
  0.7.0). A 0.6 `stop` ends only the supervisor: its terminal (`sot-capsule run`) and the agent under it live on, so a
  start over such a folder would run a second agent beside the old one. The folder is known by its layout (an entry
  named `voyages`, `drawer.voyage`, `supervisor-journal` or `rollout-evidence.json`, or `drawer.voyage.` and more; a
  0.7 folder costs one directory read), and one function, called by the launch (`spawn_detached_supervisor`, before
  anything starts) and by `end_run` (before it judges a row; a Cleanup boot ends rows without resuming them), does
  the rest under the row's guard. It dials the old supervisor's frozen lane (`supervisor-<h>.sock`, the pipe on Windows;
  the frames are restated in the file, with the two 0.6 fixtures beside it as test data) through the challenge,
  sending nothing to a peer that does not prove. A `ready` run is asked to end (`end_run` on the voyage its status
  names; a starting or ending supervisor is waited out, 90 s for the whole wait). A supervisor resting after an end is
  decided before it is stopped: it is the retire's own end when the 0.6 journal holds a closed or verified `end_run` on
  that voyage with the retire's operation id (`sot-retire-`) and no other, and otherwise a person's, which is written
  as the row's `ended` (`mark_end_requested`) before `stop`, since the live supervisor is the only witness of an end
  that no journal holds. Then `stop` (5 s for its reply), the connection dropped, and 20 s for the process to exit,
  else it is re-verified and terminated and given 10 s more. On Linux the row's scopes are then ended, whether or not a
  supervisor answered: the supervisor's own, captured before it is stopped, and every one the durable `row-scopes`
  record remembers, so a retry after a daemon's death still ends a descendant that left the process group; a scope that
  will not empty is an error. Last, with the row socket silent, every 0.6 terminal's `writer.lock` is tried and dropped
  at once (a held one is an error naming it, since a terminal that outlives its supervisor is 0.6's design and only
  the person can end it), a requested end the journal holds is made durable before anything moves, and the layout moves
  into `.retired-voyages/`. The fence is taken for that step only when `supervisor.lock` exists: a folder that holds
  the layout and no fence file never started, and the retire never creates the start marker (it moves the layout
  without the creating fence). An end the retire asked for is not a person's, so such a row starts fresh and resumes
  the conversation; an end a person asked for under 0.6 is kept, so the row rests until selected: it is read from the
  live supervisor's rest (decided before the stop, above) or, when none lives, from the 0.6 journal (a closed or
  verified `end_run` on the pointer's voyage whose id is not the retire's; a completion entry that is present and not a
  regular file, or anything else that cannot be read, rests the row, since a wrong rest is undone by one selection and
  a wrong start cannot be undone). A retire that fails leaves the row `unreachable`, retried at every start, never
  latched `terminal`. The boot resume retires every such row at once, each in its own task, before the semaphore that
  bounds spawns: a retire spawns nothing, so twenty 0.6 rows cost the slowest one (about 135 s worst case, a few
  seconds usually), plus the resume. Limits: ends are in waves of `LANE_CONCURRENCY` (a close or a Cleanup boot that
  meets 0.6 rows ends them four at a time inside `SHUTDOWN_BOUND`; a row not ended by the bound is counted not ended,
  stays registered and is retired at the next start); a descendant of a 0.6 agent that left its process group is ended
  only on Linux by the scope, on Windows by the old ConPTY job and on macOS by the process-group hangup (0.6's own
  limit); on macOS the retire inherits platform's residual that a kill by number may meet a reused number
  (`challenge_macos.rs`: about 10^5 process creations between two adjacent syscalls), and nowhere promises an
  unconditional proven-process kill there; an unregistered 0.6 folder is never retired and the whole-folder sweep keeps
  its rules; between the install and its row's retire a 0.6 session's `send` and `poll` fail (`FAILED`, exit 1) and never file mail or
  move a cursor, and `start` prints its refusal, while an identity lookup may still answer the row's own old name from its
  self file; rows of main builds
  from 6a to 6c-1c1 are not read; a 0.6 window's attach-only drawer (an opt-in the product never wrote, Windows only)
  ran a capsule on the state root itself, which no 0.7 code ends: it ends at the next reboot or by hand.
- No start begins once the close has begun: `Workspaces::begin_start` refuses after `close_gate_and_settle`, which waits
  for the starts already admitted (`gate.rs`).
- A row's phase follows its current supervisor, run by run. The phase cell is written by the observer through
  `Workspace::apply_phase_observation` (and `begin_supervisor_epoch` when a watch names a new supervisor), and by the
  watchdog's terminal marks (`apply_phase_observation` for the supervisor the watch record names, `latch_terminal`
  under the guard). `apply_phase_observation` rejects an observation from a supervisor other than the cell's epoch and
  latches `terminal` (for its supervisor) and `ended_no_respawn` (for its run: a supervisor's runs order by their ids, a
  resting supervisor reports the run it rests on, a lower or unnamed run is a stale observation and a strictly newer run
  releases the latch; a supervisor restarted onto a requested end names no run, and the latch on no run is released by
  its first run: the capsule's `ended` record carries no run, by decision, so the cell never compares runs across
  supervisors). Each row's one observer task holds the row's id and one watch on its supervisor
  (`supervisor_client::watch`: the status at the first reply, then recorded changes of phase or run, and the exit
  code before a controlled exit, within capsule guarantee 11's limits, including a derived `ready` skipped when
  an end is admitted first), and is the writer of every report: each is fed to the row registered under the id,
  so a refreshed row's phase follows its new supervisor, and a hello that names a supervisor other than the cell's
  begins its epoch. A start's settle writes nothing and wakes the observer; a settle returns at the supervisor's first
  answer, so a start reads `starting` until the agent has lived 2 s. Redials follow platform's `Redial` (250 ms doubling
  to 4 s, cut short by a wake), after an absent fence file, a failed dial or a watch's end alike, so a fence that appears
  with no wake is found. Between a start's return and the observer's first report (milliseconds) the cell keeps its
  earlier phase.
- A state folder had a supervisor iff it holds the fence file `supervisor.lock`: on Unix the daemon claims it before it
  forks and a supervisor takes it before anything else, so such a folder whose row socket does not answer reads
  `unreachable`, not `stopped`, including a start whose fork failed after the claim, and the boot resume and the
  watchdog's recheck act on it. A folder without the file reads `stopped` (never started). Nothing that only observes a
  row creates the file: `end_run` under the row's guard (every caller holds it, and no start begins without it) reads a
  missing file as never started and takes nothing; only the state-folder sweep and retirement, which work on folders no
  registered row names (the boot resume reads registered rows only), and the 0.6 retire, which works on a folder that
  holds the file, take the fence with the creating API; the retire of a folder with the 0.6 layout and no fence file
  takes no fence, so it never creates the marker. Limit
  (Windows): the supervisor is spawned before it takes its fence (`spawn/detach.rs`), so one that exits in that window
  leaves no marker for the recheck or the boot resume.

The daemon's calls on a row's supervisor are ops of the row socket, through `sot_capsule::attach_client::supervisor_client`
(`query_status`, `end_run`, `reset`, `stop`, each bounded, and the observer's `watch`); a row of another row number
is `foreign` and one that answers no such op, or none, is `unreachable`. An outcome the call could not settle is
settled by reading the same supervisor's status, never by asking again, within capsule guarantee 10's component
budgets: a lost end is `Ended` when a nonterminal status names another run or none, or the supervisor is resting
even on the same run; terminal means failed. A lost reset is `Ok` only when status names a new run, different from
the one read before the request; an absent run is not success. Resting or terminal without a new run is an error,
and what stays unsettled is `NotEnded` (an `end_run`) or an error (a `reset`), so the row is kept and lifecycle's or
the caller's retry reads the true state. Management shares
the row's 8 connections with the viewers; a dial that gets no slot is one failed observation, never an end.

Ends:
- A row is reported ended only after a confirmed end or the destroy proof: the supervisor fence can be taken (or no
  fence file exists, which proves never started) and nothing answers on the row's socket (`run::end_run`); an already
  resting supervisor is stopped and its scopes ended without the proof, and a stop that fails is logged, not an error.
  Then its comm entries are pruned, its toml removed durably, the row unregistered and
  `workspace.changed` sent. `workspace.destroy` renames a non-default row's state folder to `.closed-<id>` and removes
  it on a thread; a row the end API ends keeps its folder for a later start's sweep; the default row ends its run and
  stays registered.
- Lifecycle ends rows only through the end API (`end.rs`): `end_every` ends the rows it is given
  concurrently, at most `LANE_CONCURRENCY` at a time, tries a refused end again once a second until the deadline it is
  given, and reports each row ended, not ended, or to forget; `forget` drops the rows a past end could not remove. With
  `run::resume::resume_all` and `Workspaces::close_gate_and_settle` that is all lifecycle calls.
- The start sweep removes every tombstone, and removes a state folder no registry names only on a whole load of every
  registry, when nothing runs in it, it holds no `row-scopes` record and nothing in it was written for 24 hours; a link
  at or inside the `workspaces` folder is never followed (`state_folder.rs`).
  It also removes, in every row folder, registered or not, the `.retired-voyages/` a 0.6 retire made once its own time
  is 24 hours old (creating it and each move into it set the time, so the age counts from the last move; the sweep never
  looks inside a registered row's folder otherwise). A registered row's is removed only under the row's guard (a retire
  holds it while it fills the folder; busy is skipped until the next start) and only when no 0.6 layout name is left in
  the row's folder; a failure is tried again at the next start, and a link of that name is kept.

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

Does not: run the agent's terminal or keep its screen (capsule); decide when rows end (lifecycle asks); read a screen for
mail or type into a prompt (messaging's wake); build an agent's argv (agents); write the registry's address-book fields
or deliver mail (messaging); judge a turn's text (the turn auditor's Haiku call nudges the session; only the session's
own stamp changes a fact).

Crate: not drawn in this pass. Rows' target dependencies are capsule, agents, sidecars, files, messaging, containment
and platform; capsule (`sot-capsule`), containment and platform are crates, and agents, sidecars, files and messaging are
not. A `sot-rows` crate needs those four as crates first (the server's reply type, `HandlerOutput`, is wire's already).

Platforms: Linux runs the durable parent and tries a systemd scope per row (`spawn/row_scope.rs`), the one way a
destroy ends a child that left the agent's process group. When a transient scope is unavailable, a supervisor can
start without one and the durable parent can remain in the daemon's cgroup, so those processes can end with the
daemon's cgroup (capsule guarantee 3). macOS runs the durable parent and no scope; an agent that ignores SIGHUP can
outlive a killed supervisor (capsule guarantee 2). Windows creates the supervisor directly, trying breakaway from
the daemon's job; when that job forbids breakaway it retries inside the job, so the supervisor cannot outlive it.
A held fence is the supervisor's own exit 70. `sotd work-state` and the dialog tick run on all three. The scripts run under bash 3.2 and
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
- `state_folder.rs`: `close_row_folder`, the start sweep (`sweep_with`, `sweep_state_folders`), `retire`, `finish_retired_voyages`,
  `is_row_folder_name`, `AGE_FLOOR`.

## Start here
`mod.rs` for the structs, then `registry.rs::insert` for how a row enters. A phase change:
`workspace.rs::apply_phase_observation`. A row's handle: `handle.rs`. A work-state write: `work_state/mod.rs::apply_event`.
