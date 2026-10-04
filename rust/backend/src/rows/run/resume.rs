//! Boot resume: after a daemon restart, resume every registered capsule row whose voyage pointer exists, and log stray state directories.

use super::activation::resume_locked;
use crate::rows::Workspaces;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// ADR 0042 L1a (Codex review findings 10/11): "a small semaphore over
/// spawns" — the SAME fixed-width bound reused for both the startup
/// resume-scan's concurrent spawns and `workspace.list`'s concurrent
/// lane queries, rather than two independently-invented numbers.
pub const LANE_CONCURRENCY: usize = 4;

/// On daemon startup: resume every REGISTERED capsule workspace's
/// supervisor whose voyage pointer has ALREADY been published (rule
/// B, shrink round). A row with NO published pointer — never started
/// at all, OR a leg that crashed before ever publishing one (the
/// exact pre-pointer crash window ADR 0041 names) — is SKIPPED
/// entirely: `pty.open`'s start-on-attach (`ensure_started`) is what
/// starts those now, not this scan. Before rule B this scan launched
/// `--resume` unconditionally for EVERY registered row, including
/// ones with no pointer at all — `sot-capsule supervise --resume`
/// against a workspace with no leg to adopt and no pointer to found
/// one against simply fails (exit 69), leaving a bare state directory
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
/// `supervisor.lock`, and no watchdog is installed for a process this
/// daemon did not itself launch (decision 33: "a watchdog exists only
/// for a `Child` the daemon launched"). If that authority later goes
/// quiet, the next attach's own `resume_if_absent`/`ensure_started`
/// spawns and watches a FRESH leg under the SAME row's guard —
/// reactive, same as every other resume path (a list never resumes).
/// A genuinely unreachable row spawns `--resume`, settling before
/// this task returns (`resume_locked` -> `start_supervisor` ->
/// `settle_after_spawn`).
///
/// A state directory with NO matching registry entry is left
/// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
/// workspace list is the list" — an orphan is not addressable
/// through any op, so resuming it would create a live, unaddressable
/// process; deleted, the bare-shell fallback an earlier version used
/// here).
///
/// Runs off the startup critical path (finding 10): `server/mod.rs`
/// calls this via `tokio::spawn`, never awaited, and every probe/spawn
/// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
/// a semaphore — thousands of preserved workspaces cannot turn this
/// into an unbounded synchronous fan-out before the listener binds.
pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
    // 2026-09-04 amendment: the inert default anchor is never resumed
    // here either, even on the rare box where a pointer already exists
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

    // Covers both boot load and adoption of a still-live prior authority.
    for ws in &capsule_rows {
        super::observer::ensure_running(&workspaces, ws);
    }

    let candidates: Vec<(String, String, PathBuf, String, String)> = capsule_rows
        .into_iter()
        .filter(|ws| {
            let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
            sot_log::supervisor::journal::pointer::pointer_path(&state_dir).is_file()
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

    let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
    let mut joins = Vec::with_capacity(candidates.len());
    for (workspace_id, agent_kind, project_root, agent_name, slug) in candidates {
        let permit = semaphore.clone();
        let state_root = state_root.clone();
        let workspaces = workspaces.clone();
        joins.push(tokio::spawn(async move {
            let _permit = permit.acquire_owned().await;
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
        }));
    }
    for j in joins {
        let _ = j.await;
    }

    log_registryless_state_dirs(&state_root, &workspaces);
    log_orphaned_state_dirs(&state_root, &workspaces);
}

/// One log line naming every `<state-root>/workspaces/*` directory
/// with no matching registry entry — diagnostic only, never acted on
/// (see [`resume_all`]'s own doc).
fn log_registryless_state_dirs(state_root: &Path, workspaces: &Workspaces) {
    let dir = state_root.join("workspaces");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return,
        Err(e) => {
            tracing::debug!(dir = ?dir, error = %e, "capsule workspace resume-scan: could not read the state root for the registryless-directory log sweep");
            return;
        }
    };
    let known: std::collections::HashSet<String> =
        workspaces.list().into_iter().map(|ws| ws.workspace_id.clone()).collect();
    let orphans: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|id| !known.contains(id))
        .collect();
    if !orphans.is_empty() {
        tracing::warn!(
            count = orphans.len(), ids = ?orphans,
            "capsule workspace resume-scan: state directories with no matching registry entry -- \
             left untouched (ADR 0042: the workspace list is the list)"
        );
    }
}

/// The reverse of [`log_registryless_state_dirs`]: a REGISTERED
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
/// lifecycle observer's own `POLL_INTERVAL` loop, which would
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
