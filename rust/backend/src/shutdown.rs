//! shutdown.rs — ending this computer's sessions without resuming any,
//! and the signal that takes the daemon's own children down with it.
//!
//! [`end_rows`] ends every capsule row and the drawer by one deadline,
//! retrying a refused end once per second; any other row, and what it
//! could not end, is counted, never guessed. [`fired`] is the one
//! process-wide signal every child owner selects on, because
//! `kill_on_drop` does not run at `process::exit`. [`Signal::spawn`] starts
//! a child in its own containment and [`Contained`] is the kill: firing the
//! signal, or dropping the `Contained`, ends the child and everything it
//! started. [`ChildGuard`] only counts the ssh children of `hub_link` and
//! `topology_dial`, which are not contained.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use sot_protocol::ops::lease as bounds;

use crate::handlers::CapsuleDestroyOutcome;
use crate::lease::Leases;
use crate::workspaces::{Workspace, WorkspaceChanged, Workspaces};

/// How often a refused end is tried again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The end-run reason a window's close records.
const REASON: &str = "window closed";

/// How long step 4 waits for the daemon's own children.
const CHILDREN_WAIT: Duration = Duration::from_secs(3);

/// `SOT_TEST_SHUTDOWN_BOUND_MS` overrides [`bounds::SHUTDOWN_BOUND`] for
/// tests, read once per process; unset in every real deployment.
pub(crate) fn shutdown_bound() -> Duration {
    static OVERRIDE_MS: OnceLock<Option<u64>> = OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_SHUTDOWN_BOUND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms.map(Duration::from_millis).unwrap_or(bounds::SHUTDOWN_BOUND)
}

/// The shutdown (1.4), after step 1 stopped the accepting; it never
/// returns. Step 0 is a backstop thread that exits 1 at the bound, for
/// the next start to finish. Steps 2 and 3 share the rows deadline,
/// `decided + bound - SHUTDOWN_TAIL`; the tail is not scaled with an
/// overridden bound, because steps 4 to 6 take as long either way. Then the daemon's own children, the final
/// record, the waiting closer's answer, and exit 0.
pub(crate) async fn run(
    leases: Arc<Leases>,
    workspaces: Workspaces,
    ws_events: broadcast::Sender<WorkspaceChanged>,
    decided: Instant,
) -> ! {
    let bound = shutdown_bound();
    let backstop = bound.saturating_sub(decided.elapsed());
    std::thread::spawn(move || {
        std::thread::sleep(backstop);
        tracing::error!("shutdown still running after {bound:?}: exiting 1; the next start finishes it");
        std::process::exit(1);
    });
    leases.begin_close();
    tracing::info!("shutting down: ending this computer's sessions");

    let rows_deadline = decided + bound.saturating_sub(bounds::SHUTDOWN_TAIL);
    let gate = workspaces.clone();
    let gate_deadline = std::time::Instant::now() + rows_deadline.saturating_duration_since(Instant::now());
    let settled = tokio::time::timeout_at(
        rows_deadline,
        tokio::task::spawn_blocking(move || gate.close_gate_and_settle(gate_deadline)),
    )
    .await;
    if !matches!(settled, Ok(Ok(true))) {
        tracing::warn!("shutdown: a run start was still in flight at the rows deadline");
    }

    let report = match sot_log::state_dir::sot_state_dir() {
        Some(state_root) => end_rows(workspaces.list(), &workspaces, &ws_events, &state_root, rows_deadline).await,
        None => {
            // No state root: nothing can be ended, so every row is counted.
            let rows = workspaces.list().len() as u32;
            tracing::warn!(rows, "shutdown: no state root, so no row was ended; all are counted not ended");
            EndReport { not_ended: rows, ..Default::default() }
        }
    };

    fire();
    let children = Instant::now() + CHILDREN_WAIT;
    while live_children() > 0 && Instant::now() < children {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if live_children() > 0 {
        tracing::warn!(live = live_children(), "shutdown: the daemon's own children were still alive after {CHILDREN_WAIT:?}");
    }

    if let Err(e) = leases.finish_shutdown(report.not_ended, report.forget.clone()) {
        tracing::error!("shutdown: the final held record was not written: {e}");
    }
    if leases.closer_waits() {
        let wait = bounds::LEASE_REPLY_WAIT * 2 + bounds::NOTICE_ACK_WAIT;
        let _ = tokio::time::timeout(wait, leases.answered()).await;
    }
    tracing::info!(ended = report.ended.len(), not_ended = report.not_ended, "shutdown complete");
    std::process::exit(bounds::EXIT_REQUESTED_SHUTDOWN)
}

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
    /// Already removed by another end: not ours to end, and not counted.
    Gone,
    /// Not confirmed ended by the deadline.
    Not,
}

/// End `rows`, and the drawer iff its pointer exists (#17), without
/// resuming any: concurrently at `resume_all`'s limit, each
/// retried while refused until `deadline`. An end still running at the
/// deadline is abandoned and counted not ended; it dies with the process.
/// A row of any runtime but capsule is counted not ended and never
/// touched: the product never runs `pkill` or `tmux kill-server`.
pub(crate) async fn end_rows(
    rows: Vec<Arc<Workspace>>,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    state_root: &Path,
    deadline: Instant,
) -> EndReport {
    let drawer = drawer_is_target(state_root);
    let anchor = workspaces.default_id();
    let limit = Arc::new(tokio::sync::Semaphore::new(crate::capsule_workspace::LANE_CONCURRENCY));
    let mut joins = Vec::with_capacity(rows.len() + 1);
    for ws in rows {
        let is_anchor = anchor.as_deref() == Some(ws.workspace_id.as_str());
        let (limit, workspaces, ws_events) = (limit.clone(), workspaces.clone(), ws_events.clone());
        joins.push((ws.workspace_id.clone(), tokio::spawn(async move {
            if ws.runtime != "capsule" {
                tracing::warn!(workspace_id = %ws.workspace_id, runtime = %ws.runtime, "window closed: a row this end cannot end; it stays registered and running");
                return Ended::Not;
            }
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_row(&ws, is_anchor, &workspaces, &ws_events, deadline).await
        })));
    }
    if drawer {
        let (limit, state_root) = (limit.clone(), state_root.to_path_buf());
        joins.push(("the drawer".to_string(), tokio::spawn(async move {
            let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, limit.acquire_owned()).await else {
                return Ended::Not;
            };
            end_drawer(state_root, deadline).await
        })));
    }
    join_by(deadline, joins).await
}

/// Every target's end, each awaited no later than `deadline`: one still
/// running then is abandoned and counted not ended.
async fn join_by(deadline: Instant, joins: Vec<(String, tokio::task::JoinHandle<Ended>)>) -> EndReport {
    let mut report = EndReport::default();
    for (target, j) in joins {
        match tokio::time::timeout_at(deadline, j).await {
            Ok(Ok(Ended::Row(id))) => report.ended.push(id),
            Ok(Ok(Ended::Forget(id))) => report.forget.push(id),
            Ok(Ok(Ended::Drawer | Ended::Gone)) => {}
            Ok(Ok(Ended::Not)) | Ok(Err(_)) => report.not_ended += 1,
            Err(_) => {
                tracing::warn!(%target, "window closed: its end was still running at the deadline; not ended");
                report.not_ended += 1;
            }
        }
    }
    report
}

/// The drawer is a target iff its pointer exists in the state root (#17).
fn drawer_is_target(state_root: &Path) -> bool {
    sot_log::pointer::pointer_path(state_root).is_file()
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
        confirmed(outcome).map(|ours| ours.then_some(held))
    })
    .await;
    let held = match ended {
        Ok(Some(held)) => held,
        Ok(None) => {
            tracing::info!(workspace_id = %ws.workspace_id, "window closed: already removed by another end");
            return Ended::Gone;
        }
        Err(detail) => {
            tracing::warn!(workspace_id = %ws.workspace_id, %detail, "window closed: row not ended by the deadline; it stays registered and running");
            return Ended::Not;
        }
    };
    if is_anchor {
        let end =
            crate::handlers::end_default_row_run(workspaces, ws_events, &ws.workspace_id, &ws.slug, &agent_name, true, held);
        if tokio::time::timeout_at(deadline, end).await.is_err() {
            tracing::warn!(workspace_id = %ws.workspace_id, "window closed: the anchor's end_default_row_run was still running at the deadline; not ended");
            return Ended::Not;
        }
        return Ended::Row(ws.workspace_id.clone());
    }
    // The ended agent cannot run its own comm-leave; prune its registry
    // rows, as `workspace.destroy` does.
    let (reg_agent, reg_ws, reg_host) =
        (agent_name.clone(), ws.workspace_id.clone(), crate::workspaces::declared_host());
    let prune = tokio::task::spawn_blocking(move || {
        crate::handlers::remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    });
    if tokio::time::timeout_at(deadline, prune).await.is_err() {
        tracing::warn!(workspace_id = %ws.workspace_id, "window closed: the comm prune was still running at the deadline; not ended");
        return Ended::Not;
    }
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
                Ok(Ok(o)) => confirmed(crate::handlers::capsule_destroy_outcome_of(o)).map(|_| ()),
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

/// A confirmed end is done (`true`), and so is a row another end already
/// removed (`false`: not ours); a kept one is tried again.
fn confirmed(outcome: CapsuleDestroyOutcome) -> Result<bool, String> {
    match outcome {
        CapsuleDestroyOutcome::Removable(_) => Ok(true),
        CapsuleDestroyOutcome::AlreadyRemoved => Ok(false),
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

/// One shutdown signal, the count of children still alive under it, and the
/// trees it will kill when it fires. The process has one ([`fire`],
/// [`fired`], [`process`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    live: AtomicUsize,
    /// Every contained child's tree by id; `None` once fired, so a spawn
    /// after the fire is killed at once.
    trees: Mutex<Option<HashMap<u64, crate::contain::Tree>>>,
    next: AtomicU64,
}

impl Signal {
    pub(crate) fn new() -> Self {
        Signal {
            fired: watch::channel(false).0,
            live: AtomicUsize::new(0),
            trees: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(0),
        }
    }

    /// The flag goes first, so an owner that sees its child die already
    /// reads [`is_fired`](Self::is_fired); then every tree is killed, by
    /// dropping the registry, whatever each owner is awaiting.
    pub(crate) fn fire(&self) {
        self.fired.send_replace(true);
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(trees);
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    pub(crate) fn guard(&'static self) -> ChildGuard {
        self.live.fetch_add(1, Ordering::SeqCst);
        ChildGuard(&self.live)
    }

    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Start `cmd` in its own containment and register the tree. The owner
    /// keeps the returned [`Contained`] beside the `Child`: dropping it, or
    /// [`fire`](Self::fire), kills the child and everything it started. Once
    /// the signal has fired the child is killed at once and refused.
    pub(crate) fn spawn(
        &'static self,
        cmd: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, Contained)> {
        crate::contain::prepare(cmd);
        let mut child = cmd.spawn()?;
        let tree = match crate::contain::adopt(&child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.start_kill();
                return Err(e);
            }
        };
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let refused = {
            let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
            match trees.as_mut() {
                Some(map) => {
                    map.insert(id, tree);
                    None
                }
                None => Some(tree),
            }
        };
        if let Some(tree) = refused {
            drop(tree);
            return Err(std::io::Error::other("the daemon is shutting down"));
        }
        Ok((child, Contained { _live: self.guard(), id, sig: self }))
    }
}

/// One contained child, alive until dropped. Dropping it kills the child's
/// tree if the shutdown has not already, then stops counting the child.
pub(crate) struct Contained {
    sig: &'static Signal,
    id: u64,
    _live: ChildGuard,
}

impl Drop for Contained {
    fn drop(&mut self) {
        let tree = self
            .sig
            .trees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|map| map.remove(&self.id));
        drop(tree);
    }
}

/// The daemon's one signal; every production owner is given this.
pub(crate) fn process() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(Signal::new)
}

/// Fire the shutdown: every contained child's tree is killed. It stays fired for the life of the process.
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
/// `Child`, so it drops once the child is reaped. [`Contained`] holds one;
/// the uncontained ssh owners (`hub_link`, `topology_dial`) hold it directly.
pub(crate) struct ChildGuard(&'static AtomicUsize);

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
        assert_eq!(got, Ok(true));

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
        assert!(!drawer_is_target(root.path()), "no pointer, no drawer to end");

        std::fs::write(sot_log::pointer::pointer_path(root.path()), "x").expect("pointer");
        assert!(drawer_is_target(root.path()), "a drawer pointer makes the drawer an end target");
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

    #[tokio::test(start_paused = true)]
    async fn end_rows_returns_by_the_shared_deadline() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let joins = vec![
            ("stuck".to_string(), tokio::spawn(std::future::pending::<Ended>())),
            ("done".to_string(), tokio::spawn(async { Ended::Row("done".into()) })),
        ];
        let report = tokio::time::timeout_at(deadline + Duration::from_secs(1), join_by(deadline, joins))
            .await
            .expect("end_rows did not return by its deadline");
        assert_eq!(report, EndReport { ended: vec!["done".into()], not_ended: 1, forget: Vec::new() });
    }

    #[tokio::test]
    async fn non_capsule_row_is_counted_not_ended() {
        let root = tempfile::tempdir().expect("tempdir");
        let reg = Workspaces::new();
        let mut row = Workspace::from_label("tm", PathBuf::from("/p/tm"), false, "none".into(), String::new(), String::new());
        row.runtime = "tmux".to_string();
        reg.insert(row);
        let (events, _rx) = broadcast::channel(4);
        let report = end_rows(reg.list(), &reg, &events, root.path(), Instant::now() + Duration::from_secs(3)).await;
        assert_eq!(report.not_ended, 1, "a row the shutdown cannot end was not counted");
    }

    #[tokio::test]
    async fn already_removed_row_is_not_kept() {
        let reg = Workspaces::new();
        let mut row = Workspace::from_label("gone", PathBuf::from("/p/gone"), false, "none".into(), String::new(), String::new());
        row.runtime = "capsule".to_string();
        let row = reg.insert(row);
        // Another end removed it first.
        let _ = reg.remove_by_id(&row.workspace_id);
        let (events, _rx) = broadcast::channel(4);
        let started = Instant::now();
        let ended = end_row(&row, false, &reg, &events, started + Duration::from_secs(3)).await;
        assert_eq!(ended, Ended::Gone);
        assert!(started.elapsed() < RETRY_EVERY, "an already-removed row was retried");
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

    /// A pid is gone once a probe fails.
    #[cfg(unix)]
    fn gone(pid: i32) -> bool {
        (0..150).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes the pid.
            unsafe { libc::kill(pid, 0) != 0 }
        })
    }

    /// An owner stuck writing to a child that never reads cannot poll the
    /// signal; the fire still takes the child and its grandchild.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_kills_a_tree_whose_owner_is_blocked() {
        use tokio::io::AsyncWriteExt;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 3104 & echo $!; exec sleep 3104"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let (mut child, contained) = signal.spawn(&mut cmd).expect("spawn");
        assert_eq!(signal.live(), 1);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(child.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let grandchild: i32 = line.trim().parse().expect("grandchild pid");
        let mut stdin = child.stdin.take().unwrap();
        let owner = tokio::spawn(async move {
            let _contained = contained;
            let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
            let _ = child.wait().await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal.fire();
        tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the blocked owner did not return")
            .expect("owner task");
        assert!(gone(grandchild), "the grandchild survived the shutdown");
        assert_eq!(signal.live(), 0);
    }

    /// A spawn after the fire is killed at once and refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_after_fire_is_refused_and_killed() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        signal.fire();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("f");
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &format!("echo $$ > {}; exec sleep 3105", pid_file.display())]);
        assert!(signal.spawn(&mut cmd).is_err(), "a spawn after the fire was accepted");
        assert_eq!(signal.live(), 0);
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&pid_file).map(|s| s.trim().is_empty()).unwrap_or(true) {
            if began.elapsed() > Duration::from_secs(2) {
                return; // killed before it wrote its pid: nothing left to check
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(gone(pid), "the refused child survived");
    }
}
