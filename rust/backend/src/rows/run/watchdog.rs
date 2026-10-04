//! The watchdog: one task per daemon-spawned supervisor that classifies its exit and restarts a crash within a budget.

use super::observer::observe_with_adoption;
use super::probe::phase_of;
use super::start::settle_after_spawn;
use super::UNREACHABLE_PHASE;
use crate::rows::spawn::detach::{spawn_detached_supervisor, StartMode};
use crate::rows::Workspaces;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Child;

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

/// `sot-capsule supervise`'s own clean-exit code (`EXIT_CLEAN`).
const EXIT_CLEAN: i32 = 0;
/// `sot-capsule supervise`'s own terminal-failure exit code
/// (`EXIT_TERMINAL`) — unconditionally terminal to
/// [`wait_and_classify`], never restarted (rule F, shrink round).
const EXIT_TERMINAL: i32 = 69;
/// `sot-capsule supervise`'s own fence-contention exit code
/// (`sot_log::supervisor::EXIT_CONTENDED` — see that const's own doc
/// for the full reasoning): the authority fence was already held by
/// a LIVE supervisor. Distinct from [`EXIT_TERMINAL`] in
/// [`wait_and_classify`] — NEVER a failure of this workspace's own
/// run, only proof some other leg (almost always the previous
/// authority for this SAME state dir, still finishing its own
/// teardown) currently holds the fence.
const EXIT_CONTENDED: i32 = 70;

/// What one leg's exit means for the watchdog's own decision —
/// ADR 0042 L1a, Codex review finding 6; rule F (shrink round)
/// simplified this from three outcomes to two.
enum LegOutcome {
    /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
    /// restarted — the lane (or its absence) already says
    /// everything a client needs.
    Clean,
    /// Exit 69 (`EXIT_TERMINAL`): terminal, UNCONDITIONALLY — never
    /// restarted, regardless of whether the lane still answers. Rule
    /// F: the OLD "does the lane still answer" discriminator (a
    /// dropped `ForeignFence` outcome) tried to tell apart "lost the
    /// race for `supervisor.lock`" from "a genuinely exhausted
    /// producer", but `sot-capsule supervise` already runs its OWN
    /// internal flap/retry budget (`FLAP_THRESHOLD`,
    /// `respawn_or_terminal` in `rust/log/src/supervisor/`) before
    /// it ever chooses to exit 69 — so a second restart layer on top,
    /// here, is always redundant at best. At worst it actively hid a
    /// real failure: a producer that will NEVER recover (e.g.
    /// `claude` missing from the daemon's PATH) burned the WHOLE
    /// daemon-side restart budget (`MAX_RESTARTS_PER_WINDOW` attempts
    /// against `RESTART_WINDOW`) before finally reaching this same
    /// terminal mark anyway — "the supervisor's own three legs, not a
    /// rolling restart loop."
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
    /// Anything else: a genuine crash needing the restart sequence.
    Crash,
}

/// Maps a confirmed (or absent) exit code to the watchdog's own
/// outcome vocabulary.
fn classify_exit_code(code: Option<i32>) -> LegOutcome {
    match code {
        Some(EXIT_CLEAN) => LegOutcome::Clean,
        Some(EXIT_TERMINAL) => LegOutcome::Terminal,
        Some(EXIT_CONTENDED) => LegOutcome::Contended,
        _ => LegOutcome::Crash,
    }
}

/// Waits for `child` to end and classifies the result.
/// `tokio::process::Child::wait` is trusted outright: the daemon is
/// the sole, unambiguous owner of a supervisor it spawned itself —
/// ADR 0043 decision 33, "a watchdog exists only for a `Child` the
/// daemon launched." There is no adopted twin any more: an authority
/// `resume_all` merely finds already alive at boot is never watched
/// at all (see that function's own doc); if it later goes quiet, the
/// next attach's `resume_if_absent`/`ensure_started` spawns and
/// watches a FRESH leg, which this function then does own.
async fn wait_and_classify(mut child: Child, workspace_id: &str) -> LegOutcome {
    let code = match child.wait().await {
        Ok(status) => status.code(),
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

/// The identity a settle learned, if its lane answered at all — the
/// ONE place a daemon-spawned supervisor's identity now comes from
/// (supervisor-epoch ruling: the supervisor authors it, this daemon
/// only learns it over the lane, on every platform). Every caller
/// OVERWRITES with this, never merely sets: a settle that came back
/// anything other than `Phase` must clear the previous leg's
/// identity too, or a later terminal mark could be credited to a
/// prior, now-dead spawn.
pub(super) fn identity_of(observation: &crate::rows::workspace::Observation) -> Option<crate::rows::workspace::SupervisorIdentity> {
    match observation {
        crate::rows::workspace::Observation::Phase { supervisor, .. } => Some(*supervisor),
        _ => None,
    }
}

/// Mints an ownership token for one watchdog install — unique for
/// this daemon's lifetime, which is the whole guarantee
/// `Workspace::watchdog_owner` needs (see that field's own doc).
fn next_watchdog_owner() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A watchdog's exit-classification observation; no guard needed.
/// `Some` judges the mark against the identity the leg's own settle
/// learned, exactly as before. `None` — a leg that exited before
/// ever answering its lane, which `sot-capsule supervise` does on
/// every bootstrap failure (it returns `EXIT_TERMINAL` from three
/// sites ahead of its own accept loop) — marks
/// [`crate::rows::workspace::Observation::TerminalUnclaimed`] instead, so
/// the row still latches `terminal` rather than reading `stopped`
/// and re-spawning the same instant failure on every attach.
fn observe_terminal(workspaces: &Workspaces, workspace_id: &str, identity: Option<crate::rows::workspace::SupervisorIdentity>) {
    if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
        let observation = match identity {
            Some(supervisor) => {
                crate::rows::workspace::Observation::Phase { phase: crate::rows::workspace::Phase::Terminal, supervisor, voyage: None }
            }
            None => crate::rows::workspace::Observation::TerminalUnclaimed,
        };
        super::observer::observe(&ws, observation);
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

/// The watchdog itself: waits for the leg to exit, classifies it, and
/// on a crash restarts with `--resume` under ADR 0041's own launcher
/// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
/// WINDOW` within `RESTART_WINDOW`), then observes the workspace `Terminal` (latched — `workspace.list` reads
/// it from memory, rule F). A `Contended` leg (decision 33) logs and returns outright.
///
/// ADR 0043 decision 33: "a watchdog exists only for a `Child` the
/// daemon launched" — `child` starts as [`spawn_and_watch`]'s own
/// freshly-spawned process, and every SUBSEQUENT leg (a crash
/// restart) is a fresh spawn too. There is no adopted counterpart —
/// see `resume_all`'s own doc for what an already-alive authority
/// gets instead (nothing, until it goes quiet and a fresh attach
/// resumes and watches it). `None` from [`Workspaces::capsule_guard`]
/// at entry (the row already gone by the time this task got to ask)
/// means there is nothing to watch at all.
///
/// R4a: `Terminal` reports immediately, no guard, identity-judged; `Crash` holds the guard across recheck/backoff/spawn.
pub(super) fn install_watchdog(
    workspace_id: String,
    sot_capsule_exe: PathBuf,
    state_dir: PathBuf,
    argv: Vec<String>,
    cwd: PathBuf,
    agent_name: String,
    slug: String,
    child: Child,
    initial_identity: Option<crate::rows::workspace::SupervisorIdentity>,
    workspaces: Workspaces,
) {
    tokio::spawn(async move {
        let Some(capsule_guard) = workspaces.capsule_guard(&workspace_id) else {
            return;
        };
        // Ruling: the watchdog is the single writer of restarts for a
        // child this daemon spawned. This guard is the ONE place the
        // row's `watchdog_owner` fact is announced (constructor) and
        // retracted (Drop) -- a compare-and-clear against THIS
        // task's own token, never a plain clear, so a superseded
        // watchdog's belated cleanup can never erase a replacement's
        // ownership set after it. One token per task, minted once:
        // ownership is a property of the WATCHDOG, not of whichever
        // leg it currently holds, so a respawn has nothing to
        // announce here.
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
        let _watchdog_owner_guard = WatchdogOwnerGuard::new(workspaces.clone(), workspace_id.clone());
        // What each exit classification is judged against: the
        // identity the CURRENT leg's own settle learned, or `None`
        // when its lane never answered.
        let mut current_identity = initial_identity;
        let mut leg_opt = Some(child);
        let mut restart_times: Vec<Instant> = Vec::new();
        loop {
            let outcome = match leg_opt.take() {
                Some(c) => wait_and_classify(c, &workspace_id).await,
                // A previous restart attempt itself found nothing to
                // wait on -- counts as another crash against the
                // same budget.
                None => LegOutcome::Crash,
            };
            match outcome {
                LegOutcome::Clean => return,
                LegOutcome::Terminal => {
                    tracing::warn!(
                        workspace_id = %workspace_id,
                        "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                    );
                    observe_terminal(&workspaces, &workspace_id, current_identity);
                    return;
                }
                LegOutcome::Contended => {
                    tracing::info!(
                        workspace_id = %workspace_id,
                        "capsule supervisor watchdog: leg exited contended (70) -- another authority holds the fence; leaving the row for the next attach"
                    );
                    return;
                }
                LegOutcome::Crash => {
                    // Decided before taking the guard -- giving up needs no recheck (R4a).
                    let now = Instant::now();
                    restart_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                    if restart_times.len() >= MAX_RESTARTS_PER_WINDOW {
                        tracing::error!(
                            workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                            "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                        );
                        observe_terminal(&workspaces, &workspace_id, current_identity);
                        return;
                    }
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
                    let backoff = RESTART_BACKOFFS[restart_times.len().min(RESTART_BACKOFFS.len() - 1)];
                    tracing::warn!(
                        workspace_id = %workspace_id, backoff = ?backoff, attempt = restart_times.len() + 1,
                        "capsule supervisor watchdog: crashed, restarting with --resume"
                    );
                    tokio::time::sleep(backoff).await;
                    restart_times.push(Instant::now());
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
                    let argv_for_spawn = argv.clone();
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
                        .map(|child| (child, permit))
                    })
                    .await;
                    match spawn_result {
                        Ok(Ok((child, permit))) => {
                            // Settle BEFORE this guard drops — the
                            // SAME shared wait `spawn_and_watch`
                            // itself uses; see `settle_after_spawn`'s
                            // own doc for why a fresh spawn cannot
                            // skip this without reopening the exact
                            // guard-release race this restart's own
                            // recheck above just closed. A fresh leg
                            // begins a fresh epoch exactly as the
                            // first spawn does: by being adopted
                            // from what it answers.
                            let settle_dir = state_dir.clone();
                            let settle_wsid = workspace_id.clone();
                            let settled = tokio::task::spawn_blocking(move || settle_after_spawn(&settle_dir, &settle_wsid)).await;
                            // Always OVERWRITE, never merely set: a
                            // settle that yields no `Phase` clears
                            // the PREVIOUS leg's identity, or the
                            // next terminal mark would be credited
                            // to a spawn that is already dead.
                            current_identity = match &settled {
                                Ok((_phase, observation)) => identity_of(observation),
                                Err(_join_err) => None,
                            };
                            if let (Ok((_phase, observation)), Some(ws)) = (settled, workspaces.resolve(Some(&workspace_id))) {
                                observe_with_adoption(&ws, observation);
                            }
                            drop(permit);
                            leg_opt = Some(child);
                        }
                        Ok(Err(e)) if e.kind() == ErrorKind::Unsupported => {
                            // `qualified_state_root` refused (ADR 0043
                            // decision 23: the state root went unqualified
                            // out from under a live row -- an `XDG_STATE_HOME`
                            // change, a remounted volume). No retry can change
                            // that without operator action -- mark terminal
                            // now (the error names the recovery).
                            tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: unqualified state root -- marking terminal, no restart");
                            observe_terminal(&workspaces, &workspace_id, current_identity);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_budget_numbers_match_adr_0041s_own_launcher_table() {
        assert_eq!(RESTART_BACKOFFS.len(), 5);
        assert_eq!(MAX_RESTARTS_PER_WINDOW, 5);
        assert_eq!(RESTART_WINDOW, std::time::Duration::from_secs(60));
        assert_eq!(
            RESTART_BACKOFFS.map(|d| d.as_secs()),
            [1, 3, 7, 15, 30]
        );
    }
}
