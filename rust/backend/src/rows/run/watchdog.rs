//! The watchdog: one task per daemon-spawned supervisor that classifies its exit and restarts a crash within a budget.

use super::observer::observe;
use super::probe::phase_of;
use super::start::settle_after_spawn;
use super::{NEVER_STARTED_PHASE, UNREACHABLE_PHASE};
use crate::rows::spawn::detach::{spawn_detached_supervisor, Spawn, SupervisorChild};
use crate::rows::Workspaces;
use sot_log::supervisor::{StartMode, EXIT_AGENT_ENDED, EXIT_CLEAN, EXIT_CONTENDED, EXIT_TERMINAL};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// ADR 0042 L1a (Codex review finding 6): the daemon's own watchdog
/// restart budget for a capsule supervisor — ADR 0041's own launcher
/// restart sequence ("restart with `--resume` on the launcher's shipped
/// 1/3/7/15/30 s sequence, at most 5 restarts in 60 s, then stop and
/// report"). The daemon has become that launcher for every capsule
/// workspace it creates or resumes, so this is the ADR's own row, not
/// new policy.
pub const RESTART_BACKOFFS: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(7),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(30),
];

pub const MAX_RESTARTS_PER_WINDOW: usize = 5;

pub const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// The backoff before restart number `restarts` (from 0): `RESTART_BACKOFFS`, or, with the test seam
/// `SOT_TEST_BACKOFF_MS` named, that many milliseconds for every restart (the budget is a count, so a test that waits it
/// out proves the same with short waits). Inert unless named.
fn backoff_for(restarts: usize) -> std::time::Duration {
    match std::env::var("SOT_TEST_BACKOFF_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        Some(ms) => std::time::Duration::from_millis(ms),
        None => RESTART_BACKOFFS[restarts.min(RESTART_BACKOFFS.len() - 1)],
    }
}

/// The restart budget, one per watchdog. An agent that cannot start is a count, not a rate: consecutive restarts of
/// supervisors that never reached `ready` are counted without a window, so how long each start, settle or life takes
/// never decides it, and the count resets when one reaches `ready`. Rows knows `ready` by the supervisor's word: its
/// watch told `ready` (the agent lived 2 s) before it ended; one that died before any watch named it, or whose agent
/// ended sooner, never did. Restarts of supervisors that did
/// reach `ready` keep `MAX_RESTARTS_PER_WINDOW` within `RESTART_WINDOW`. Either way the exit after the fifth restart
/// is the give-up.
#[derive(Default)]
struct Budget {
    never_ready: usize,
    ready_restarts: Vec<Instant>,
    last_exit_was_ready: bool,
}

impl Budget {
    /// Judges an exit of a supervisor that was (or never was) told `ready`: the backoff before its restart, `None`
    /// when the budget is spent.
    fn on_exit(&mut self, said_ready: bool, now: Instant) -> Option<std::time::Duration> {
        self.last_exit_was_ready = said_ready;
        let restarts = if self.last_exit_was_ready {
            self.never_ready = 0;
            self.ready_restarts.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
            self.ready_restarts.len()
        } else {
            self.never_ready += 1;
            self.never_ready - 1
        };
        (restarts < MAX_RESTARTS_PER_WINDOW).then(|| backoff_for(restarts))
    }

    /// The restart judged by the last exit began.
    fn restarted(&mut self, now: Instant) {
        if self.last_exit_was_ready {
            self.ready_restarts.push(now);
        }
    }

    /// The restart's ordinal, for the log.
    fn attempt(&self) -> usize {
        if self.last_exit_was_ready { self.ready_restarts.len() + 1 } else { self.never_ready }
    }
}

/// What one leg's exit means for the watchdog's own decision —
/// ADR 0042 L1a, Codex review finding 6; rule F (shrink round)
/// simplified this from three outcomes to two.
enum LegOutcome {
    /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
    /// restarted — the lane (or its absence) already says
    /// everything a client needs.
    Clean,
    /// Exit 69 (`EXIT_TERMINAL`): the supervisor cannot serve the row (its folders, its lane, the run's own failure, a
    /// reset that failed), terminal UNCONDITIONALLY -- never restarted, regardless of whether the lane still answers.
    /// The supervisor does not retry the agent itself: an agent that cannot run reaches this watchdog as 75 or 76, and
    /// this watchdog's budget (`MAX_RESTARTS_PER_WINDOW` within `RESTART_WINDOW`) is the one restart budget, ending in
    /// the same terminal mark.
    Terminal,
    /// Exit 70 (`EXIT_CONTENDED`): the authority fence was already
    /// held by a LIVE supervisor when this leg tried to acquire it —
    /// almost always the previous authority for this SAME state dir,
    /// still finishing its own teardown. NEVER treated as
    /// [`Terminal`] (that would mark a perfectly healthy workspace
    /// terminal out from under a run some OTHER leg is still
    /// actively serving). ADR 0043 decision 33 (shrink round): no
    /// longer re-probed for adoption either — [`install_watchdog`]
    /// logs and returns, leaving the row for the next attach's own
    /// [`resume_if_absent`]/[`ensure_started`] to find and resume
    /// under the row's guard, same as any other quiet lane.
    Contended,
    /// Exit 76 (`EXIT_AGENT_ENDED`): the agent started and ended by itself, with no end asked for. Restarted like a
    /// crash, within the same budget; kept apart so the log names it.
    AgentEnded,
    /// Anything else, 75 (`EXIT_AGENT_NOT_STARTED`: the agent could not be spawned) and a killed supervisor included:
    /// a genuine crash needing the restart sequence, with the same argv.
    Crash,
}

/// Maps a confirmed (or absent) exit code to the watchdog's own
/// outcome vocabulary.
fn classify_exit_code(code: Option<i32>) -> LegOutcome {
    match code {
        Some(EXIT_CLEAN) => LegOutcome::Clean,
        Some(EXIT_TERMINAL) => LegOutcome::Terminal,
        Some(EXIT_CONTENDED) => LegOutcome::Contended,
        Some(EXIT_AGENT_ENDED) => LegOutcome::AgentEnded,
        _ => LegOutcome::Crash,
    }
}

/// What starts a watchdog's loop: a child this daemon launched, whose wait it trusts outright (the daemon is its durable
/// parent, so on Unix the exit is reported), or the exit of a supervisor it did not launch, which the observer's watch
/// carried (the code `row.exiting` told, none for a kill). Every later leg is the watchdog's own child.
pub(super) enum First {
    Child(SupervisorChild),
    Exited(Option<i32>),
}

/// Waits for `child` to end and classifies the result. A parent lost before it could report is a crash here: the
/// restart's own recheck (`watchdog_may_act`) finds out whether anything is still serving.
async fn wait_and_classify(child: SupervisorChild, workspace_id: &str) -> LegOutcome {
    let code = match child.wait().await {
        Ok(code) => code,
        Err(e) => {
            tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
            return LegOutcome::Crash;
        }
    };
    classify_exit_code(code)
}

/// Whether it is still THIS watchdog's business to act on
/// `workspace_id`, checked under the row's own guard (the caller
/// proves it by already holding it) before EITHER mutation the
/// watchdog can make: a restart, or a terminal mark. Rechecks, now
/// that the guard is actually held, that the row is still
/// registered, not already marked terminal, AND still genuinely
/// unreachable (`phase_of`). The window this closes is real, not
/// merely theoretical: `wait_and_classify` itself takes no lock, so
/// between a leg's confirmed exit and this watchdog's own task
/// actually reaching the guard, a stale attach's own
/// `ensure_started`/`resume_if_absent` (on a separate blocking-pool
/// thread — genuinely concurrent with this async task on a
/// multi-worker runtime) can win the guard FIRST and resume the row
/// itself, installing its own fresh watchdog. Any of the three false
/// means some OTHER actor already settled this row's fate while this
/// watchdog waited — a restart would then spawn a REDUNDANT
/// authority, and a terminal mark would misreport a row a fresh
/// authority is already serving. BLOCKING (`phase_of`): callers run
/// it via `spawn_blocking`.
fn watchdog_may_act(workspace_id: &str, state_dir: &Path, workspaces: &Workspaces) -> bool {
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        tracing::debug!(workspace_id = %workspace_id, "capsule supervisor watchdog: row no longer registered; stopping");
        return false;
    };
    if ws.phase() == crate::rows::workspace::Phase::Terminal {
        return false;
    }
    if phase_of(state_dir) != UNREACHABLE_PHASE {
        tracing::debug!(
            workspace_id = %workspace_id,
            "capsule supervisor watchdog: row was already resumed by another actor while this watchdog waited for the guard; not acting"
        );
        return false;
    }
    true
}

/// Mints an ownership token for one watchdog install — unique for
/// this daemon's lifetime, which is the whole guarantee
/// `Workspace::watchdog_owner` needs (see that field's own doc).
fn next_watchdog_owner() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Whether a phase is one nothing answers: `unreachable`, or `stopped`, which only Windows can read of a first start
/// whose supervisor has not yet taken its fence.
fn nothing_answers(phase: &str) -> bool {
    phase == UNREACHABLE_PHASE || phase == NEVER_STARTED_PHASE
}

/// Whether it is still this watchdog's business to mark `workspace_id` terminal without a supervisor's word, checked under
/// the row's guard: the row is registered, not already terminal, and nothing answers (see [`nothing_answers`]). A terminal
/// mark on a row a fresh supervisor is serving would misreport it. BLOCKING (`phase_of`): callers run it via
/// `spawn_blocking`.
fn may_mark_terminal(workspace_id: &str, state_dir: &Path, workspaces: &Workspaces) -> bool {
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        return false;
    };
    ws.phase() != crate::rows::workspace::Phase::Terminal && nothing_answers(phase_of(state_dir))
}

/// The give-up's and the unwatched 69's terminal mark, under the guard the caller holds: latched only if
/// [`may_mark_terminal`].
async fn latch_terminal_if_nothing_answers(workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) {
    let (dir, wsid, registry) = (state_dir.to_path_buf(), workspace_id.to_string(), workspaces.clone());
    let may = tokio::task::spawn_blocking(move || may_mark_terminal(&wsid, &dir, &registry)).await.unwrap_or(false);
    if may {
        if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
            ws.latch_terminal();
        }
    }
}

/// The one place the row's `watchdog_owner` fact is announced (constructor) and retracted (Drop): a compare-and-clear
/// against this watchdog's own token, never a plain clear, so a superseded watchdog's belated cleanup can never erase a
/// replacement's ownership set after it. One token per watchdog, minted once: ownership is a property of the watchdog, not
/// of whichever leg it currently holds, so a respawn has nothing to announce.
struct WatchdogOwnerGuard {
    workspaces: Workspaces,
    workspace_id: String,
    token: u64,
}

impl WatchdogOwnerGuard {
    fn new(workspaces: Workspaces, workspace_id: String) -> Self {
        let token = next_watchdog_owner();
        if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
            ws.set_watchdog_owner(token);
        }
        Self { workspaces, workspace_id, token }
    }
}

impl Drop for WatchdogOwnerGuard {
    fn drop(&mut self) {
        if let Some(ws) = self.workspaces.resolve(Some(&self.workspace_id)) {
            ws.clear_watchdog_owner_if(self.token);
        }
    }
}

/// Test-only barrier at the top of the watchdog's own Crash-arm
/// restart attempt, BEFORE it ever takes the row's guard: when
/// `SOT_TEST_ACTIVATION_BARRIER` is set, blocks until the test
/// creates `<that path>.watchdog-restart` -- a file SEPARATE from
/// the main barrier, so a test can hold the watchdog and
/// `pty.open`'s own activation independently and so prove either
/// lock ordering deterministically (Codex review round 6 SHOULD-FIX:
/// replace an uncontrolled race with exactly this). No-op in
/// production; gives up past a generous bound rather than hang a
/// forgotten release forever.
async fn wait_for_test_watchdog_restart_barrier() {
    let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    let path = std::path::PathBuf::from(format!("{barrier_path}.watchdog-restart"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !path.is_file() {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(path = ?path, "watchdog restart test barrier: released by timeout, not by the test");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The watchdog itself: waits for the supervisor to exit (or takes the exit the observer's watch carried), classifies it,
/// and on a crash restarts with `--resume` under ADR 0041's own launcher restart sequence (`RESTART_BACKOFFS`, at most
/// `MAX_RESTARTS_PER_WINDOW` within `RESTART_WINDOW`), then marks the workspace `Terminal` (latched, `workspace.list` reads
/// it from memory). A `Contended` leg logs and returns outright.
///
/// `first` is [`spawn_and_watch`]'s own freshly spawned child, or the exit of a supervisor this daemon found alive at boot
/// and the observer's watch saw end ([`First::Exited`]); every later leg (a crash restart) is a fresh child of the
/// watchdog. `None` from [`Workspaces::capsule_guard`] at entry (the row already gone) means there is nothing to watch. The
/// row's `watchdog_owner` is announced before this returns.
///
/// `Terminal` reports immediately for a supervisor the watch named, judged by its identity; `Crash` holds the guard across
/// recheck/backoff/spawn.
#[allow(clippy::too_many_lines, reason = "the per-row watchdog task: recheck, back off and respawn; predates the 100-line limit")]
pub(super) fn install_watchdog(
    workspace_id: String,
    sot_capsule_exe: PathBuf,
    state_dir: PathBuf,
    argv: Vec<String>,
    cwd: PathBuf,
    agent_name: String,
    slug: String,
    first: First,
    workspaces: Workspaces,
) {
    // Ownership is announced here, before this returns and so under the caller's guard: a task that announced it would leave
    // the row ownerless for a moment, and every resume path defers to an owner.
    let Some(capsule_guard) = workspaces.capsule_guard(&workspace_id) else {
        return;
    };
    let watchdog_owner_guard = WatchdogOwnerGuard::new(workspaces.clone(), workspace_id.clone());
    tokio::spawn(async move {
        let _watchdog_owner_guard = watchdog_owner_guard;
        let mut first = Some(first);
        let mut budget = Budget::default();
        loop {
            let outcome = match first.take() {
                Some(First::Child(c)) => wait_and_classify(c, &workspace_id).await,
                Some(First::Exited(code)) => classify_exit_code(code),
                // A previous restart attempt itself found nothing to
                // wait on -- counts as another crash against the
                // same budget.
                None => LegOutcome::Crash,
            };
            let agent_ended = matches!(outcome, LegOutcome::AgentEnded);
            match outcome {
                LegOutcome::Clean => return,
                LegOutcome::Terminal => {
                    tracing::warn!(
                        workspace_id = %workspace_id,
                        "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                    );
                    // The supervisor the row's watch named is marked on its own word; a supervisor no watch named (it
                    // exited 69 before it answered, as every bootstrap failure does) is marked under the guard, if
                    // nothing answers, so the row latches `terminal` rather than reading `stopped` and starting the same
                    // instant failure on every attach.
                    match workspaces.resolve(Some(&workspace_id)).and_then(|ws| ws.watched().map(|watched| (ws, watched))) {
                        Some((ws, watched)) => observe(
                            &ws,
                            crate::rows::workspace::Observation::Phase {
                                phase: crate::rows::workspace::Phase::Terminal,
                                supervisor: watched.supervisor,
                                run: None,
                            },
                        ),
                        None => {
                            let _held = capsule_guard.lock().await;
                            latch_terminal_if_nothing_answers(&workspaces, &workspace_id, &state_dir).await;
                        }
                    }
                    return;
                }
                LegOutcome::Contended => {
                    tracing::info!(
                        workspace_id = %workspace_id,
                        "capsule supervisor watchdog: leg exited contended (70) -- another authority holds the fence; leaving the row for the next attach"
                    );
                    return;
                }
                LegOutcome::Crash | LegOutcome::AgentEnded => {
                    // Decided before taking the guard -- giving up needs no recheck (R4a).
                    let said_ready = workspaces.resolve(Some(&workspace_id)).is_some_and(|ws| ws.watch_said_ready());
                    let Some(backoff) = budget.on_exit(said_ready, Instant::now()) else {
                        tracing::error!(
                            workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                            "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                        );
                        // Rechecked under the guard: nothing else has started the row since.
                        let _held = capsule_guard.lock().await;
                        latch_terminal_if_nothing_answers(&workspaces, &workspace_id, &state_dir).await;
                        return;
                    };
                    wait_for_test_watchdog_restart_barrier().await;
                    let _held = capsule_guard.lock().await;
                    let may_act = {
                        let dir = state_dir.clone();
                        let wsid = workspace_id.clone();
                        let workspaces = workspaces.clone();
                        tokio::task::spawn_blocking(move || watchdog_may_act(&wsid, &dir, &workspaces))
                            .await
                            .unwrap_or(false)
                    };
                    if !may_act {
                        return;
                    }
                    tracing::warn!(
                        workspace_id = %workspace_id, backoff = ?backoff, attempt = budget.attempt(),
                        agent_ended,
                        "capsule supervisor watchdog: crashed, restarting with --resume"
                    );
                    tokio::time::sleep(backoff).await;
                    budget.restarted(Instant::now());
                    // The run gate, asked after the backoff so a
                    // closing or held-back gate is read as it is now.
                    // Held through this leg's settle below.
                    let permit = match workspaces.begin_start(&workspace_id) {
                        Ok(permit) => permit,
                        Err(refusal) => {
                            tracing::info!(workspace_id = %workspace_id, %refusal, "capsule supervisor watchdog: restart refused by the run gate -- not restarting");
                            return;
                        }
                    };
                    // ADR 0043 decision 29: a process spawn never runs
                    // on a Tokio worker. Clones are the closure's OWN
                    // copies (`'static` + `Send`, required across the
                    // `.await` below) -- the loop's own locals are
                    // untouched and reused on the NEXT iteration.
                    let exe = sot_capsule_exe.clone();
                    let dir = state_dir.clone();
                    let cwd_for_spawn = cwd.clone();
                    let agent_name_for_spawn = agent_name.clone();
                    let workspace_id_for_spawn = workspace_id.clone();
                    let slug_for_spawn = slug.clone();
                    // Accounts brief + ADR 0046 decision 6: read at
                    // RESTART time, never captured at install time.
                    // `workspace.reauth` moves a live row's account
                    // while its watchdog is parked on the OLD leg, so
                    // a captured pair would respawn on the login the
                    // row no longer has -- a live leg under a record
                    // that says otherwise. Same resolve, same
                    // `unwrap_or_default` degradation, as the first
                    // leg's in `spawn_and_watch`.
                    let (agent_kind_for_spawn, account_for_spawn) = workspaces
                        .resolve(Some(&workspace_id))
                        .map(|ws| (ws.agent(), ws.account()))
                        .unwrap_or_default();
                    // The row's own recipe, built now like the agent and account above, never the captured argv
                    // whole: a reauth's `--resume <id>` belongs to its own leg only.
                    let argv_for_spawn = restart_argv(
                        crate::agents::argv::agent_argv(&agent_kind_for_spawn, Some(cwd_for_spawn.as_path())),
                        &argv,
                    );
                    // The supervisor about to start has been watched by no one.
                    if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
                        ws.clear_watched();
                    }
                    let spawn_result = tokio::task::spawn_blocking(move || {
                        spawn_detached_supervisor(
                            &permit,
                            &exe,
                            &dir,
                            StartMode::Resume,
                            &argv_for_spawn,
                            &cwd_for_spawn,
                            &agent_name_for_spawn,
                            &workspace_id_for_spawn,
                            &slug_for_spawn,
                            &agent_kind_for_spawn,
                            &account_for_spawn,
                        )
                        .map(|spawn| (spawn, permit))
                    })
                    .await;
                    match spawn_result {
                        Ok(Ok((Spawn::Contended, _permit))) => {
                            // The row's fence is held: a live authority, or another birth's claim. Exactly the
                            // Contended exit (70): leave the row for the next attach's admission.
                            tracing::info!(
                                workspace_id = %workspace_id,
                                "capsule supervisor watchdog: restart found the authority fence claimed -- leaving the row for the next attach"
                            );
                            return;
                        }
                        Ok(Ok((Spawn::Started(child), permit))) => {
                            // Settle BEFORE this guard drops -- the
                            // SAME shared wait `spawn_and_watch`
                            // itself uses; see `settle_after_spawn`'s
                            // own doc for why a fresh spawn cannot
                            // skip this without reopening the exact
                            // guard-release race this restart's own
                            // recheck above just closed. The fresh
                            // supervisor is then the observer's to
                            // watch.
                            let settle_dir = state_dir.clone();
                            let settle_wsid = workspace_id.clone();
                            let _ = tokio::task::spawn_blocking(move || settle_after_spawn(&settle_dir, &settle_wsid)).await;
                            workspaces.wake_observer(&workspace_id);
                            drop(permit);
                            first = Some(First::Child(child));
                        }
                        Ok(Err(e)) if e.kind() == ErrorKind::Unsupported => {
                            // `qualified_state_root` refused (ADR 0043
                            // decision 23: the state root went unqualified
                            // out from under a live row -- an `XDG_STATE_HOME`
                            // change, a remounted volume). No retry can change
                            // that without operator action -- mark terminal
                            // now (the error names the recovery).
                            tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: unqualified state root -- marking terminal, no restart");
                            latch_terminal_if_nothing_answers(&workspaces, &workspace_id, &state_dir).await;
                            return;
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                        }
                        Err(join_err) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %join_err, "capsule supervisor watchdog: restart spawn task panicked");
                        }
                    }
                    // `_held` drops here -- released only once the
                    // new leg exists, or the attempt has failed.
                }
            }
        }
    });
}

/// The argv a watchdog restart spawns: the row's recipe as built now (`recipe`), else the spawn's argv without a
/// reauth's `--resume <id>` pair, which belongs to the reauth's own leg only. For an ordinary row the two are the same.
fn restart_argv(recipe: Result<Vec<String>, String>, spawned: &[String]) -> Vec<String> {
    recipe.unwrap_or_else(|error| {
        tracing::warn!(%error, "capsule supervisor watchdog: the row's recipe could not be built; restarting with the spawn's argv, its resume removed");
        let mut argv = Vec::with_capacity(spawned.len());
        let mut rest = spawned.iter();
        while let Some(arg) = rest.next() {
            if arg == "--resume" {
                rest.next();
            } else {
                argv.push(arg.clone());
            }
        }
        argv
    })
}

#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod tests;
