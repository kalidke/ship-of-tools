//! shutdown.rs — ending this computer's sessions without resuming any,
//! and the signal that takes the daemon's own children down with it.
//!
//! [`end_rows`] ends every capsule row and the drawer by one deadline,
//! retrying a refused end once per second; what it could not end is
//! counted, never guessed. [`fired`] is the one process-wide signal every
//! child owner selects on, because `kill_on_drop` does not run at
//! `process::exit`; [`ChildGuard`] counts the children still alive.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use crate::handlers::CapsuleDestroyOutcome;
use crate::workspaces::{Workspace, WorkspaceChanged, Workspaces};

/// How often a refused end is tried again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The end-run reason a window's close records.
const REASON: &str = "window closed";

/// What [`end_rows`] did. `ended` and `forget` are both ended runs;
/// `forget` holds the rows whose registration files would not go, for the
/// next start to drop (#26). The anchor's row is kept on purpose and shows
/// in `ended`. The drawer has no workspace id, so it shows only in
/// `not_ended`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EndReport {
    pub ended: Vec<String>,
    pub not_ended: u32,
    pub forget: Vec<String>,
}

/// One target's end.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Ended; its registration is gone, or (the anchor) kept.
    Row(String),
    /// Ended, but its registration files would not go.
    Forget(String),
    Drawer,
    /// Not confirmed ended by the deadline.
    Not,
}

/// End every capsule row, and the drawer iff its pointer exists (#17),
/// without resuming any: concurrently at `resume_all`'s limit, each
/// retried while refused until `deadline`. An end still running at the
/// deadline is abandoned and counted not ended; it dies with the process.
pub(crate) async fn end_rows(
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    state_root: &Path,
    deadline: Instant,
) -> EndReport {
    let (rows, drawer) = targets(workspaces, state_root);
    let anchor = workspaces.default_id();
    let limit = Arc::new(tokio::sync::Semaphore::new(crate::capsule_workspace::LANE_CONCURRENCY));
    let mut joins = Vec::with_capacity(rows.len() + 1);
    for ws in rows {
        let is_anchor = anchor.as_deref() == Some(ws.workspace_id.as_str());
        let (limit, workspaces, ws_events) = (limit.clone(), workspaces.clone(), ws_events.clone());
        joins.push(tokio::spawn(async move {
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_row(&ws, is_anchor, &workspaces, &ws_events, deadline).await
        }));
    }
    if drawer {
        let (limit, state_root) = (limit.clone(), state_root.to_path_buf());
        joins.push(tokio::spawn(async move {
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_drawer(state_root, deadline).await
        }));
    }
    let mut report = EndReport::default();
    for j in joins {
        match j.await {
            Ok(Ended::Row(id)) => report.ended.push(id),
            Ok(Ended::Forget(id)) => report.forget.push(id),
            Ok(Ended::Drawer) => {}
            Ok(Ended::Not) | Err(_) => report.not_ended += 1,
        }
    }
    report
}

/// Every capsule row, and whether the drawer is a target: it is iff its
/// pointer exists in the state root (#17).
fn targets(workspaces: &Workspaces, state_root: &Path) -> (Vec<Arc<Workspace>>, bool) {
    let rows = workspaces.list().into_iter().filter(|ws| ws.runtime == "capsule").collect();
    (rows, sot_log::pointer::pointer_path(state_root).is_file())
}

/// One row, as `workspace.destroy` ends it but with no resume first (#11).
/// The anchor's row is kept, as the default row's own end keeps it.
async fn end_row(
    ws: &Workspace,
    is_anchor: bool,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    deadline: Instant,
) -> Ended {
    let (agent, agent_name) = (ws.agent(), ws.agent_name());
    let (agent_ref, name_ref) = (agent.as_str(), agent_name.as_str());
    let ended = retry_until(deadline, move || async move {
        let (outcome, held) = crate::handlers::destroy_capsule_workspace(
            &ws.workspace_id,
            REASON,
            agent_ref,
            name_ref,
            &ws.slug,
            &ws.project_root,
            workspaces,
            false,
        )
        .await;
        confirmed(outcome).map(|()| held)
    })
    .await;
    let held = match ended {
        Ok(held) => held,
        Err(detail) => {
            tracing::warn!(workspace_id = %ws.workspace_id, %detail, "window closed: row not ended by the deadline; it stays registered and running");
            return Ended::Not;
        }
    };
    if is_anchor {
        crate::handlers::end_default_row_run(workspaces, ws_events, &ws.workspace_id, &ws.slug, &agent_name, true, held)
            .await;
        return Ended::Row(ws.workspace_id.clone());
    }
    // The ended agent cannot run its own comm-leave; prune its registry
    // rows, as `workspace.destroy` does.
    let (reg_agent, reg_ws, reg_host) =
        (agent_name.clone(), ws.workspace_id.clone(), crate::workspaces::declared_host());
    let _ = tokio::task::spawn_blocking(move || {
        crate::handlers::remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    })
    .await;
    let slug = ws.slug.clone();
    let ended = forget_unless_removed(ws.workspace_id.clone(), deadline, || crate::handlers::remove_row_files(&slug)).await;
    let _ = workspaces.remove_by_id(&ws.workspace_id);
    drop(held);
    let _ = ws_events.send(WorkspaceChanged {
        action: "destroyed".into(),
        slug: ws.slug.clone(),
        workspace_id: ws.workspace_id.clone(),
    });
    ended
}

/// The drawer: `end_run` on the state root itself, which stops its
/// authority after a confirmed end.
async fn end_drawer(state_root: PathBuf, deadline: Instant) -> Ended {
    // Canonicalized as `destroy_capsule_workspace` does, and for its reason.
    let (state_root, root_canonicalized) = match state_root.canonicalize() {
        Ok(canonical) => (canonical, true),
        Err(_) => (state_root, false),
    };
    let ended = retry_until(deadline, || {
        let state_root = state_root.clone();
        async move {
            match tokio::task::spawn_blocking(move || {
                crate::capsule_workspace::end_run(&state_root, REASON, root_canonicalized)
            })
            .await
            {
                Ok(Ok(o)) => confirmed(crate::handlers::capsule_destroy_outcome_of(o)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(join_err) => Err(format!("end_run task panicked: {join_err}")),
            }
        }
    })
    .await;
    match ended {
        Ok(()) => Ended::Drawer,
        Err(detail) => {
            tracing::warn!(%detail, "window closed: the drawer was not ended by the deadline; it stays running");
            Ended::Not
        }
    }
}

/// A confirmed end is done; a kept one is tried again.
fn confirmed(outcome: CapsuleDestroyOutcome) -> Result<(), String> {
    match outcome {
        CapsuleDestroyOutcome::Removable(_) => Ok(()),
        CapsuleDestroyOutcome::Kept { detail } => Err(detail),
    }
}

/// An ended row's registration files, removed with retries until
/// `deadline`. Files that would not go put the id in `forget`, for the
/// next start to drop (#26).
async fn forget_unless_removed(id: String, deadline: Instant, mut remove: impl FnMut() -> bool) -> Ended {
    let removed = retry_until(deadline, || {
        let removed = remove();
        async move {
            if removed {
                Ok(())
            } else {
                Err("registration file not removed".to_string())
            }
        }
    })
    .await;
    match removed {
        Ok(()) => Ended::Row(id),
        Err(_) => Ended::Forget(id),
    }
}

/// Run `attempt` until it succeeds, once per second while it refuses,
/// until `deadline`. An attempt still running at the deadline is dropped;
/// whatever blocking work it started runs on, unwaited. `Err` carries the
/// last refusal.
async fn retry_until<T, F, Fut>(deadline: Instant, mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut last = String::from("no attempt finished");
    loop {
        match tokio::time::timeout_at(deadline, attempt()).await {
            Ok(Ok(done)) => return Ok(done),
            Ok(Err(refused)) => last = refused,
            Err(_) => return Err(format!("still running at the deadline; last refusal: {last}")),
        }
        let next = Instant::now() + RETRY_EVERY;
        if next >= deadline {
            return Err(last);
        }
        tokio::time::sleep_until(next).await;
    }
}

/// One shutdown signal and the count of children still alive under it.
/// The process has one ([`fire`], [`fired`], [`ChildGuard::new`]).
struct Signal {
    fired: watch::Sender<bool>,
    live: AtomicUsize,
}

impl Signal {
    fn new() -> Self {
        Signal { fired: watch::channel(false).0, live: AtomicUsize::new(0) }
    }

    fn fire(&self) {
        self.fired.send_replace(true);
    }

    async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    fn guard(&'static self) -> ChildGuard {
        self.live.fetch_add(1, Ordering::SeqCst);
        ChildGuard(&self.live)
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

fn process() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(Signal::new)
}

/// Fire the shutdown: every child owner selecting on [`fired`] kills its
/// child. It stays fired for the life of the process.
pub(crate) fn fire() {
    process().fire()
}

/// Resolves once the shutdown has fired; at once if it already has.
pub(crate) async fn fired() {
    process().fired().await
}

/// Children whose [`ChildGuard`] is still alive.
pub(crate) fn live_children() -> usize {
    process().live()
}

/// Counts one child alive until dropped. Its owner keeps it beside the
/// `Child`, so it drops once the child is reaped.
pub(crate) struct ChildGuard(&'static AtomicUsize);

impl ChildGuard {
    pub(crate) fn new() -> Self {
        process().guard()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule_workspace::EndRunOutcome as O;

    #[tokio::test(start_paused = true)]
    async fn end_with_retry_table() {
        // Starting, Starting, then a confirmed end: ended.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut script = vec![O::Starting, O::Starting, O::RecordVerified].into_iter();
        let got = retry_until(deadline, || {
            let o = script.next().expect("no attempt after a confirmed end");
            async move { confirmed(crate::handlers::capsule_destroy_outcome_of(o)) }
        })
        .await;
        assert_eq!(got, Ok(()));

        // Refused forever: tried once per second, then not ended at the deadline.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut tries = 0;
        let got = retry_until(deadline, || {
            tries += 1;
            async { confirmed(crate::handlers::capsule_destroy_outcome_of(O::NotEnded("refused".into()))) }
        })
        .await;
        assert_eq!(got, Err("refused".to_string()));
        assert_eq!(tries, 10, "one attempt per second across the 10 s");
        assert!(Instant::now() + RETRY_EVERY >= deadline, "gave up before the deadline");

        // An attempt still running at the deadline: abandoned, not ended.
        let deadline = Instant::now() + Duration::from_secs(10);
        let got = retry_until(deadline, || std::future::pending::<Result<(), String>>()).await;
        assert!(got.is_err());
        assert_eq!(Instant::now(), deadline);
    }

    #[test]
    fn drawer_is_an_end_target() {
        let root = tempfile::tempdir().expect("tempdir");
        let reg = Workspaces::new();
        let mut row = Workspace::from_label("cap", PathBuf::from("/p/cap"), false, "none".into(), String::new(), String::new());
        row.runtime = "capsule".to_string();
        let row = reg.insert(row);
        let mut other = Workspace::from_label("tm", PathBuf::from("/p/tm"), false, "none".into(), String::new(), String::new());
        other.runtime = "tmux".to_string();
        reg.insert(other);

        let (rows, drawer) = targets(&reg, root.path());
        assert_eq!(rows.iter().map(|w| w.workspace_id.clone()).collect::<Vec<_>>(), vec![row.workspace_id.clone()]);
        assert!(!drawer, "no pointer, no drawer to end");

        std::fs::write(sot_log::pointer::pointer_path(root.path()), "x").expect("pointer");
        let (_, drawer) = targets(&reg, root.path());
        assert!(drawer, "a drawer pointer makes the drawer an end target");
    }

    #[tokio::test(start_paused = true)]
    async fn forget_on_unremovable_registration() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut tries = 0;
        let ended = forget_unless_removed("stuck".into(), deadline, || {
            tries += 1;
            false
        })
        .await;
        assert_eq!(ended, Ended::Forget("stuck".into()));
        assert_eq!(tries, 5, "retried until the deadline");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut tries = 0;
        let ended = forget_unless_removed("late".into(), deadline, || {
            tries += 1;
            tries == 3
        })
        .await;
        assert_eq!(ended, Ended::Row("late".into()), "removed on a retry is not forgotten");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_guard_killed_on_fire() {
        // A private signal: firing the process's own would kill every other
        // test's children.
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut child = tokio::process::Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let guard = signal.guard();
        assert_eq!(signal.live(), 1);
        let owner = tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {
                _ = child.wait() => {}
                _ = signal.fired() => { let _ = child.kill().await; }
            }
            child.try_wait().ok().flatten()
        });
        signal.fire();
        let status = tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the child was not reaped within 3 s")
            .expect("owner task");
        assert!(status.is_some(), "the child was killed but not reaped");
        assert_eq!(signal.live(), 0);
    }
}
