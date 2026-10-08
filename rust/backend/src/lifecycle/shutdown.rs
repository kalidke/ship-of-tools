//! shutdown.rs — ending this computer's sessions without resuming any,
//! and the one terminal every controlled daemon exit goes through.
//!
//! [`end_rows`] ends every capsule row and the drawer by one deadline,
//! retrying a refused end once per second; any other row, and what it
//! could not end, is counted, never guessed. The daemon's own children
//! receive checked termination requests through [`super::child_signal`]:
//! [`exit`] fires its signal before the one raw process exit, and does not
//! wait for any tree to die.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::Instant;

use sot_protocol::ops::lease as bounds;

use crate::rows::run::end::CapsuleDestroyOutcome;
use crate::lifecycle::lease::Leases;
use crate::rows::{Workspace, WorkspaceChanged, Workspaces};

/// How often a refused end is tried again.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The end-run reason a window's close records.
const REASON: &str = "window closed";

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
/// returns. Step 0 is a backstop thread that exits 1 at the bound through [`exit`], for
/// the next start to finish; a stalled OS child creation or adoption can delay that exit's
/// fire. Steps 2 and 3 share the rows deadline,
/// `decided + bound - SHUTDOWN_TAIL`; the tail is not scaled with an
/// overridden bound, because steps 4 to 6 take as long either way. Then the final
/// record, the waiting closer's answer, and exit 0 through [`exit`], which fires the child signal.
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
        tracing::error!("shutdown still running after {bound:?}: exiting 1 after the child fire; the next start finishes it");
        exit(1);
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

    let report = match sot_log::host::state_dir::sot_state_dir() {
        Some(state_root) => end_rows(workspaces.list(), &workspaces, &ws_events, &state_root, rows_deadline).await,
        None => {
            // No state root: nothing can be ended, so every row is counted.
            let rows = workspaces.list().len() as u32;
            tracing::warn!(rows, "shutdown: no state root, so no row was ended; all are counted not ended");
            EndReport { not_ended: rows, ..Default::default() }
        }
    };

    if let Err(e) = leases.finish_shutdown(report.not_ended, report.forget.clone()) {
        tracing::error!("shutdown: the final held record was not written: {e}");
    }
    if leases.closer_waits() {
        let wait = bounds::LEASE_REPLY_WAIT * 2 + bounds::NOTICE_ACK_WAIT;
        let _ = tokio::time::timeout(wait, leases.answered()).await;
    }
    tracing::info!(ended = report.ended.len(), not_ended = report.not_ended, "shutdown complete");
    exit(bounds::EXIT_REQUESTED_SHUTDOWN)
}

/// How long the exit waits for the fire to answer before it exits anyway.
const FIRE_WAIT: Duration = Duration::from_secs(2);

/// Fire the child signal and then take the one raw daemon exit. A controlled exit of the serving daemon (the close, the
/// backstop, an update restart, a handled signal, the main result) comes here, so each contained tree is asked to end
/// first; the first caller's code is the one the process exits with ([`terminal`]). Request errors are logged and the
/// chosen code stands; this is no wait for death, and the exit waits no longer than [`FIRE_WAIT`] for the requests either:
/// a child creation stalled in the OS holds the mutex the fire needs, and the exit, the backstop's included, does not
/// depend on it.
pub(crate) fn exit(code: i32) -> ! {
    terminal(super::child_signal::process(), code, |code| {
        std::process::exit(code)
    })
}

/// The terminal body: claim the exit, fire `signal` on a thread of its own, wait at most [`FIRE_WAIT`] for it, then hand
/// `code` to `terminate`. Two controlled ends can arrive together (the backstop and a close that finishes at the bound):
/// the first to arrive claims the exit and its code stands; the other waits for the process to end under it. A test
/// supplies a private signal and a callback.
pub(super) fn terminal<T>(
    signal: &'static super::child_signal::Signal,
    code: i32,
    terminate: impl FnOnce(i32) -> T,
) -> T {
    if !signal.claim_exit() {
        loop {
            std::thread::park();
        }
    }
    let (sent, answered) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("sotd-fire".into())
        .spawn(move || {
            let _ = sent.send(signal.fire());
        });
    let fired = match started {
        Ok(_) => answered.recv_timeout(FIRE_WAIT).ok(),
        // No thread to wait on: fire here, as the exit has no other way to ask.
        Err(error) => {
            tracing::error!(%error, "terminal child fire: no thread to run it on");
            Some(signal.fire())
        }
    };
    match fired {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            tracing::error!(%error, "terminal child fire failed");
            eprintln!("sotd: terminal child fire failed: {error}");
        }
        None => {
            tracing::error!("terminal child fire still running after {FIRE_WAIT:?}: exiting with {code} regardless");
            eprintln!("sotd: terminal child fire still running after {FIRE_WAIT:?}: exiting with {code} regardless");
        }
    }
    terminate(code)
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
    let limit = Arc::new(tokio::sync::Semaphore::new(crate::rows::run::resume::LANE_CONCURRENCY));
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
    sot_log::supervisor::journal::pointer::pointer_path(state_root).is_file()
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
        let (outcome, held) = crate::rows::run::end::destroy_capsule_workspace(
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
            crate::rows::anchor::end_default_row_run(workspaces, ws_events, &ws.workspace_id, &ws.slug, &agent_name, true, held);
        if tokio::time::timeout_at(deadline, end).await.is_err() {
            tracing::warn!(workspace_id = %ws.workspace_id, "window closed: the anchor's end_default_row_run was still running at the deadline; not ended");
            return Ended::Not;
        }
        return Ended::Row(ws.workspace_id.clone());
    }
    // The ended agent cannot run its own comm-leave; prune its registry
    // rows, as `workspace.destroy` does.
    let (reg_agent, reg_ws, reg_host) =
        (agent_name.clone(), ws.workspace_id.clone(), crate::rows::store::declared_host());
    let prune = tokio::task::spawn_blocking(move || {
        crate::comm::registry::registry::remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    });
    if tokio::time::timeout_at(deadline, prune).await.is_err() {
        tracing::warn!(workspace_id = %ws.workspace_id, "window closed: the comm prune was still running at the deadline; not ended");
        return Ended::Not;
    }
    let slug = ws.slug.clone();
    let ended = forget_unless_removed(ws.workspace_id.clone(), deadline, || crate::rows::run::end::remove_row_files(&slug)).await;
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
                crate::rows::run::end_run::end_run(&state_root, REASON, root_canonicalized)
            })
            .await
            {
                Ok(Ok(o)) => confirmed(crate::rows::run::end::capsule_destroy_outcome_of(o)).map(|_| ()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::run::end_run::EndRunOutcome as O;

    /// A child creation stalled in the OS holds the registry mutex the fire needs: the terminal hands its code on after the
    /// wait, and the fire completes when the creation does.
    #[cfg(unix)]
    #[test]
    fn the_terminal_does_not_wait_for_a_stalled_creation() {
        use crate::lifecycle::child_signal::Signal;
        use std::sync::{mpsc, Barrier};
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let released = Arc::new(Barrier::new(2));
        let (entered, arrived) = mpsc::channel();
        let hook_released = released.clone();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |_| {
            entered.send(()).unwrap();
            hook_released.wait();
        }));
        let creation = std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("sleep");
            cmd.arg("600");
            if let Ok(mut child) = signal.spawn_std(&mut cmd) {
                let _ = child.kill();
            }
        });
        arrived
            .recv_timeout(Duration::from_secs(10))
            .expect("the creation did not stall");
        let began = std::time::Instant::now();
        let code = terminal(signal, 7, |code| code);
        let waited = began.elapsed();
        released.wait();
        creation.join().unwrap();
        assert_eq!(code, 7);
        assert!(
            waited < Duration::from_secs(5),
            "the terminal waited {waited:?} for a stalled creation"
        );
        assert!(
            signal.is_fired(),
            "the permanent flag was not published before the wait"
        );
    }

    /// Two controlled ends that arrive together (the backstop and a close finishing at the bound): the first one's code is
    /// the only one handed to the exit, and the second never reaches it.
    #[test]
    fn the_first_controlled_exit_decides_the_code() {
        use crate::lifecycle::child_signal::Signal;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let handed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (first_in, first_inside) = std::sync::mpsc::channel();
        let record = handed.clone();
        let first = std::thread::spawn(move || {
            terminal(signal, 1, |code| {
                record.lock().unwrap().push(code);
                first_in.send(()).unwrap();
                // The first exit is still on its way out when the second arrives.
                std::thread::sleep(Duration::from_millis(500));
            })
        });
        first_inside
            .recv_timeout(Duration::from_secs(10))
            .expect("the first exit did not reach its end");
        let record = handed.clone();
        let _second = std::thread::spawn(move || {
            terminal(signal, 0, |code| record.lock().unwrap().push(code))
        });
        std::thread::sleep(Duration::from_millis(1000));
        first.join().unwrap();
        assert_eq!(
            *handed.lock().unwrap(),
            vec![1],
            "the second controlled exit reached the process exit"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn end_with_retry_table() {
        // Starting, Starting, then a confirmed end: ended.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut script = vec![O::Starting, O::Starting, O::RecordVerified].into_iter();
        let got = retry_until(deadline, || {
            let o = script.next().expect("no attempt after a confirmed end");
            async move { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(o)) }
        })
        .await;
        assert_eq!(got, Ok(true));

        // Refused forever: tried once per second, then not ended at the deadline.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut tries = 0;
        let got = retry_until(deadline, || {
            tries += 1;
            async { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(O::NotEnded("refused".into()))) }
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

        std::fs::write(sot_log::supervisor::journal::pointer::pointer_path(root.path()), "x").expect("pointer");
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
}
