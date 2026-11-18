//! Boot resume: after a daemon restart, resume every registered capsule row whose fence file exists, and log stray state directories.

use super::activation::resume_locked;
use super::probe::phase_of;
use super::UNREACHABLE_PHASE;
use crate::rows::Workspaces;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// ADR 0042 L1a (Codex review findings 10/11): "a small semaphore over
/// spawns" — the SAME fixed-width bound reused for both the startup
/// resume-scan's concurrent spawns and `workspace.list`'s concurrent
/// lane queries, rather than two independently-invented numbers.
pub const LANE_CONCURRENCY: usize = 4;

/// The boot resume's pace: a row whose start spawns begins at least this long after the last one that did, so the
/// heavy part of one agent's start has mostly ended before the next begins (the evidence is in `docs/defects.md`).
pub const BOOT_START_SPACING: Duration = Duration::from_secs(3);

/// A row the boot resume starts: its id, agent kind, project root, agent name and slug.
type Candidate = (String, String, PathBuf, String, String);

/// On daemon startup: resume every REGISTERED capsule workspace's
/// supervisor whose fence file EXISTS (rule B, shrink round): a
/// supervisor was started there. A row with NO fence file — never
/// started at all — is SKIPPED entirely: `pty.open`'s start-on-attach (`ensure_started`) is what
/// starts those now, not this scan. Before rule B this scan launched
/// `--resume` unconditionally for EVERY registered row, including
/// ones with no fence file at all, leaving a bare state directory
/// behind and reading back as a misleading row on the owner's own box
/// (the field finding behind this shrink round).
///
/// ADR 0043 decision 33 (Codex review, 2026-09-11): every candidate's
/// guard is taken FIRST, via `try_lock` — never blocking, and a row
/// already busy (an attach's own `ensure_started`/`resume_if_absent`
/// got there first) is simply skipped, that caller's own attempt
/// being the one that counts — and the decision itself is then made
/// UNDER that guard by [`resume_locked`], the SAME function every
/// other resume path shares: a row still alive (from a previous
/// daemon lifetime, spawned detached by ADR 0042 design, outliving
/// the daemon that just restarted) reports its own current phase and
/// spawns nothing — no second `--resume` leg races the live one's
/// `supervisor.lock`. The row's observer, started for every capsule
/// row here, holds a watch on it: the live supervisor is adopted, and
/// if it later exits the observer hands the row to a watchdog that
/// restarts it within rows' budget, with no selection (a list never
/// resumes).
/// A genuinely unreachable row spawns `--resume`, settling before
/// this task returns (`resume_locked` -> `start_supervisor` ->
/// `settle_after_spawn`).
///
/// A state directory with NO matching registry entry is never resumed (ADR 0042: "the daemon's
/// workspace list is the list" — an orphan is not addressable
/// through any op, so resuming it would create a live, unaddressable
/// process; deleted, the bare-shell fallback an earlier version used
/// here). After the resume pass the state folder sweep runs (`state_folder::sweep_state_folders`): it removes the
/// tombstones closes left, and a folder no registry on disk names only when every registry read whole, its supervisor
/// and leg are proven absent, and nothing in it was written for 24 hours; a folder it leaves is logged once.
///
/// Runs off the startup critical path (finding 10): `server/mod.rs`
/// calls this via `tokio::spawn`, never awaited, and every probe/spawn
/// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
/// a semaphore — thousands of preserved workspaces cannot turn this
/// into an unbounded synchronous fan-out before the listener binds.
///
/// A computer's boot leaves every supervisor dead, and a spawn's permit
/// ends at the supervisor's first answer, long before its agent has
/// started; so the rows are begun in the order a window's strip shows them
/// ([`in_strip_order`]), a row whose supervisor does not answer at least
/// `BOOT_START_SPACING` after the last such row ([`pace_starts`]), and
/// each lists as `queued` until its turn. A row whose supervisor answers
/// (a daemon restart's) takes no turn.
pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
    resume_paced(state_root, workspaces, BOOT_START_SPACING).await;
}

/// [`resume_all`] with the pace as a parameter: a test hook, so a test runs the whole resume at a short spacing.
async fn resume_paced(state_root: PathBuf, workspaces: Workspaces, spacing: Duration) {
    // 2026-09-04 amendment: the inert default anchor is never resumed
    // here either, even on the rare box where a fence file already exists
    // for it (a hand-edited toml that dropped its agent after a prior
    // real run). Every OTHER `agent == "none"` capsule row still
    // resumes: `agent_argv("none", None)` is a real leg (the bare platform
    // shell). The predicate — and why its runtime term matters — is
    // `Workspaces::is_inert_default_anchor`.
    let capsule_rows: Vec<Arc<crate::rows::Workspace>> = workspaces
        .list()
        .into_iter()
        .filter(|ws| ws.runtime == "capsule")
        .filter(|ws| !workspaces.is_inert_default_anchor(ws))
        .collect();

    // A selection file an earlier process left names a window that is gone; one a live connection set since survives.
    for ws in workspaces.list() {
        let _ = crate::agents::selection::clear_unowned(&state_root, &ws.workspace_id);
    }

    // Covers both boot load and adoption of a still-live prior authority.
    for ws in &capsule_rows {
        super::observer::ensure_running(&workspaces, ws);
    }

    let candidates: Vec<Candidate> = capsule_rows
        .into_iter()
        .filter(|ws| {
            let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
            super::fence_file_exists(&state_dir)
        })
        .map(|ws| {
            (
                ws.workspace_id.clone(),
                ws.agent(),
                ws.project_root.clone(),
                ws.agent_name(),
                ws.slug.clone(),
            )
        })
        .collect();

    retire_old_rows_together(&state_root, &workspaces, &candidates).await;

    let candidates = in_strip_order(&workspaces, candidates).await;
    for (workspace_id, ..) in &candidates {
        if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
            ws.set_start_queued(true);
        }
    }
    let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
    let absent = |(workspace_id, ..): &Candidate| {
        let state_dir = super::state_dir_for(&state_root, workspace_id);
        async move { tokio::task::spawn_blocking(move || phase_of(&state_dir) == UNREACHABLE_PHASE).await.unwrap_or(true) }
    };
    let begin = |(workspace_id, agent_kind, project_root, agent_name, slug): Candidate| {
        let permit = semaphore.clone();
        let state_root = state_root.clone();
        let workspaces = workspaces.clone();
        tokio::spawn(async move {
            let _permit = permit.acquire_owned().await;
            if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
                ws.set_start_queued(false);
            }
            let Some(guard) = workspaces.capsule_guard(&workspace_id) else {
                return;
            };
            let lock_result = guard.try_lock();
            match lock_result {
                Ok(_held) => {
                    let workspace_id_for_log = workspace_id.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        resume_locked(&state_root, &workspace_id, &agent_kind, &agent_name, &slug, &project_root, workspaces)
                    })
                    .await;
                    match result {
                        Ok(Ok(phase)) => {
                            tracing::info!(workspace_id = %workspace_id_for_log, phase, "capsule workspace resume-scan: row resolved");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(workspace_id = %workspace_id_for_log, error = %e, "capsule workspace resume-scan: row resolution failed");
                        }
                        Err(join_err) => {
                            tracing::warn!(workspace_id = %workspace_id_for_log, error = %join_err, "capsule workspace resume-scan: resume task panicked");
                        }
                    }
                }
                Err(_) => {
                    tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: row's guard busy; skipping");
                }
            }
        })
    };
    for j in pace_starts(candidates, spacing, absent, begin).await {
        let _ = j.await;
    }

    let (sweep_root, sweep_workspaces) = (state_root.clone(), workspaces.clone());
    let _ = tokio::task::spawn_blocking(move || crate::rows::state_folder::sweep_state_folders(&sweep_root, &sweep_workspaces)).await;
    log_orphaned_state_dirs(&state_root, &workspaces);
}

/// Begins each of `rows` in the order given: a row `absent` answers true for (nothing answers on its row socket, so its
/// turn spawns a supervisor) begins at least `spacing` after the last such row began, and a row whose supervisor
/// answers begins at once and takes no turn. `begin` starts a row and hands back its task.
async fn pace_starts<R, F>(
    rows: Vec<R>,
    spacing: Duration,
    mut absent: impl FnMut(&R) -> F,
    mut begin: impl FnMut(R) -> tokio::task::JoinHandle<()>,
) -> Vec<tokio::task::JoinHandle<()>>
where
    F: Future<Output = bool>,
{
    let mut joins = Vec::with_capacity(rows.len());
    let mut last: Option<tokio::time::Instant> = None;
    for row in rows {
        if absent(&row).await {
            if let Some(at) = last {
                tokio::time::sleep_until(at + spacing).await;
            }
            last = Some(tokio::time::Instant::now());
        }
        joins.push(begin(row));
    }
    joins
}

/// How long the order's one read of the registry may take before the rows start in the order given: a read that stalls
/// (the file system under load) must not hold every row back.
const REGISTRY_READ_BOUND: Duration = Duration::from_secs(3);

/// The candidates in the order a window's strip shows rows by default, from one read of the registry, so the rows a
/// person looks at first start first; an unreadable registry, or one not read within `REGISTRY_READ_BOUND`, keeps the
/// order given.
async fn in_strip_order(workspaces: &Workspaces, candidates: Vec<Candidate>) -> Vec<Candidate> {
    in_strip_order_from(workspaces, candidates, crate::comm::registry::registry::read_comm_agents, REGISTRY_READ_BOUND).await
}

/// [`in_strip_order`] with the registry's read and its bound as parameters: a test hook, so a test gives a read that
/// never returns.
async fn in_strip_order_from(
    workspaces: &Workspaces,
    candidates: Vec<Candidate>,
    read: impl FnOnce() -> Option<serde_json::Value> + Send + 'static,
    bound: Duration,
) -> Vec<Candidate> {
    if candidates.len() < 2 {
        return candidates;
    }
    let agents = match tokio::time::timeout(bound, tokio::task::spawn_blocking(read)).await {
        Ok(read) => read.ok().flatten(),
        Err(_) => {
            tracing::warn!("capsule workspace resume-scan: the registry was not read in time; rows start in the order given");
            None
        }
    };
    let rows = workspaces.list();
    let host = crate::rows::store::declared_host();
    strip_order(candidates, |(workspace_id, ..)| {
        rows.iter()
            .find(|ws| ws.workspace_id == *workspace_id)
            .map(|ws| crate::rows::work_state::shown_of(ws, &rows, agents.as_ref(), &host))
            .unwrap_or_default()
    })
}

/// `rows` sorted, stably, by what each shows (`shown`) as the window's strip ranks it (`activity_rank`, without its
/// result badge): a row waiting on its person, then done, working, waiting and the rest; the newest stamp first within
/// each.
fn strip_order<R>(mut rows: Vec<R>, shown: impl Fn(&R) -> crate::rows::work_state::Shown) -> Vec<R> {
    let rank = |state: &str| match state {
        "blocked" => 0u8,
        "done" => 1,
        "working" => 2,
        "waiting" => 3,
        _ => 4,
    };
    rows.sort_by_cached_key(|row| {
        let shown = shown(row);
        (rank(&shown.state), std::cmp::Reverse(shown.status_at))
    });
    rows
}

/// Every candidate whose folder holds the 0.6 layout is retired at once, each in its own task: the semaphore below bounds
/// spawns, and a retire spawns nothing (its waits are on other processes), so twenty 0.6 rows cost the slowest one, not
/// five waves of four. A row whose guard is busy is skipped (its holder's launch retires it); a retire that fails is
/// logged, and the row's resume still runs, where the launch's own retire tries again and the row reads `unreachable`.
/// Every retire is joined before the resumes start.
async fn retire_old_rows_together(
    state_root: &Path,
    workspaces: &Workspaces,
    candidates: &[Candidate],
) {
    let mut joins = Vec::new();
    for (workspace_id, ..) in candidates {
        let state_dir = super::state_dir_for(state_root, workspace_id);
        if matches!(crate::rows::spawn::retire_06::layout(&state_dir), Ok(found) if found.is_empty()) {
            continue;
        }
        let (workspace_id, workspaces) = (workspace_id.clone(), workspaces.clone());
        joins.push(tokio::spawn(async move {
            let Some(guard) = workspaces.capsule_guard(&workspace_id) else {
                return;
            };
            let Ok(_held) = guard.try_lock() else {
                tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: row's guard busy; its launch retires it");
                return;
            };
            match tokio::task::spawn_blocking(move || crate::rows::spawn::retire_06::retire_06(&state_dir)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule workspace resume-scan: row resolution failed (retiring the 0.6 row)");
                }
                Err(join_err) => {
                    tracing::warn!(workspace_id = %workspace_id, error = %join_err, "capsule workspace resume-scan: retire task panicked");
                }
            }
        }));
    }
    for j in joins {
        let _ = j.await;
    }
}

/// The reverse of the sweep above: a REGISTERED
/// capsule row whose `state_dir` does not exist under THIS daemon's
/// own state root — a DIAGNOSTIC candidate list only, never a proof.
/// A row that went through `workspace.create` and reached the
/// registry always had a `start_supervisor` call succeed there (a
/// failed spawn rolls the row and its toml back before either is
/// persisted, `rows/ops/create.rs`'s create rollback), and that success is
/// exactly what creates the directory — but a row can ALSO reach the
/// registry by a pre-seeded or hand-authored toml that has never been
/// through `workspace.create` at all (a legitimate, tested shape:
/// "Rule H" in `tests/capsule_workspaces/`'s own integration suite), and
/// reads identically here — no state dir, no pointer, "never started
/// yet" — RIGHT UP UNTIL its first attach spawns it for real. This
/// function cannot and does not try to tell the two apart (see
/// `workspace.list`'s own phase, which deliberately does not either —
/// decision 33's Rule B: "neither has ever had a real run"); it only
/// NAMES every such row, once, at boot — never from the per-row
/// lifecycle observer's own redial loop, which would
/// otherwise repeat the same line forever — so an operator staring at
/// `sotd.log` after the field defect this closes (a leaked scratch-
/// daemon registry row) has a lead to start from. Removal is still
/// only ever `workspace.destroy`'s to decide, via `end_run`'s own
/// PROOF (a real lane connect, decision 27's absent shape) — which
/// needs no such distinction either: nothing running is nothing
/// running, whether the row is freshly seeded or truly abandoned.
/// STAT STRICTLY, matching `end_run`: only `ErrorKind::NotFound` is
/// "missing" — a permission or I/O error says nothing about whether
/// the directory is actually gone, so it is skipped (not logged)
/// rather than guessed at.
fn log_orphaned_state_dirs(state_root: &Path, workspaces: &Workspaces) {
    for ws in workspaces.list() {
        if ws.runtime != "capsule" {
            continue;
        }
        let state_dir = super::state_dir_for(state_root, &ws.workspace_id);
        match std::fs::metadata(&state_dir) {
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            _ => continue,
        }
        tracing::warn!(
            workspace_id = %ws.workspace_id, slug = %ws.slug, state_dir = ?state_dir,
            "capsule workspace resume-scan: registered row has no state directory under this \
             daemon's state root -- reads as an ordinary stopped row (indistinguishable here \
             from one simply never started yet); workspace.destroy removes it once no \
             supervisor answers its lane"
        );
    }
}

#[cfg(test)]
#[path = "resume_selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "resume_pace_tests.rs"]
mod pace_tests;
