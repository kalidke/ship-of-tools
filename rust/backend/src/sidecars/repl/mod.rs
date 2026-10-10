// sidecars/repl/mod.rs — supervisor for the persistent Julia REPL child.
//
// Per ADR 0009 the REPL is a separate `julia` process the backend keeps
// alive. The frontend sends code to evaluate; the REPL responds with a
// list of frames (stdout / stderr / value / error / done).
//
// Lifecycle pattern mirrors `kernel.rs` and `mathjax.rs`: long-lived
// child, mpsc submit channel, oneshot replies, drain on death + relaunch
// on next call.
//
// Distinct from the Julia kernel (`kernel.rs`): the kernel introspects
// project state without running user code (Modules-mode, AST hashes); the
// REPL evaluates arbitrary user code in its own `Main` namespace. They're
// separate so a runaway eval can't take down kernel introspection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

pub(crate) mod execute;
pub(crate) mod lifecycle;
pub(crate) mod ops;
mod supervisor;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod restart_tests;

use lifecycle::ReplLifecycle;
use lifecycle::{LifecycleCell, SharedLifecycle};
use supervisor::{spawn_supervisor, Supervisor};

/// How long a retirement (the stop, the tree's termination request and the direct child's reap) may take before a
/// restart gives up with an error. It bounds that asynchronous wait only, not lock acquisition or the OS calls.
pub(crate) const REPL_RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// One streamed REPL frame relayed off the supervisor onto the per-backend
/// broadcast bus. The supervisor reads each `repl.frame` evt line off the
/// Julia child's stdout and fans it out here; every connection subscribes and
/// writes a `repl.frame` evt frame. Mirrors `comm::mail::bus::AgentMessage` — a
/// small Clone+Debug payload type over a `broadcast::channel`. `workspace_id`
/// is the originating workspace. `frame` is
/// the opaque `{kind, ...}` object the Julia shim emitted, passed through
/// verbatim so the protocol surface stays kernel-defined.
#[derive(Clone, Debug)]
pub struct ReplFrameMsg {
    pub eval_id: u64,
    pub workspace_id: Option<String>,
    pub frame: serde_json::Value,
}

/// Inline stdout+stderr byte budget for a collected `repl.execute` run. Past
/// this, further stdout/stderr text frames are dropped and `truncated` is set —
/// but value/image/error/done frames are always kept. Bounds memory against a
/// runaway `println` loop and keeps the response under the 1 MiB envelope cap.
pub const EXEC_TEXT_CAP: usize = 256 * 1024;

/// Loss-free per-run frame sink for `repl.execute`. The supervisor tees each
/// frame for a collected eval_id in here (in addition to the best-effort
/// broadcast bus), so a slow consumer can never `Lagged`-drop a frame the way
/// the shared 256-slot `broadcast` would. Bounded by `EXEC_TEXT_CAP`.
#[derive(Default)]
pub struct ExecAccum {
    pub frames: Vec<serde_json::Value>,
    pub text_bytes: usize,
    pub truncated: bool,
}

impl ExecAccum {
    fn push(&mut self, frame: serde_json::Value) {
        let kind = frame.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "stdout" || kind == "stderr" {
            if self.text_bytes >= EXEC_TEXT_CAP {
                self.truncated = true;
                return;
            }
            let len = frame
                .get("text")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0);
            self.text_bytes += len;
            if self.text_bytes >= EXEC_TEXT_CAP {
                self.truncated = true;
            }
        }
        self.frames.push(frame);
    }
}

pub type ExecCollector = Arc<std::sync::Mutex<ExecAccum>>;

#[derive(Clone)]
pub struct Repl {
    inner: Arc<ReplInner>,
}

struct ReplInner {
    /// The shutdown signal every supervisor of this REPL is started under and watches; supplied by the owner of
    /// this handle, never chosen inside supervision.
    sig: &'static crate::lifecycle::child_signal::Signal,
    /// The selected project, the running supervisor and the one being retired, under one lock.
    slot: Mutex<Slot>,
    /// Broadcast sink for streamed `repl.frame` evts. Threaded into every
    /// supervisor we spawn (initial + each `restart_with_project`) so frames
    /// from a fresh child still reach subscribers.
    frame_tx: broadcast::Sender<ReplFrameMsg>,
    /// The originating workspace id, stamped onto every `ReplFrameMsg` so the
    /// frontend can route frames to the right REPL drawer.
    workspace_id: Option<String>,
    /// Child lifecycle (`not_started`/`starting`/`ready`/`dead`), written by
    /// the supervisor under a spawn-generation guard and read by
    /// `workspace.list` so a precompiling first boot renders as *starting*,
    /// not dead. See `ReplLifecycle`.
    lifecycle: SharedLifecycle,
}

/// What the handle owns. `project` is the workspace directory, activated as the DEFAULT env for the persistent REPL
/// so user code runs in the session package's environment, never the `ShipToolsRepl` shim project, even before the
/// directory holds a `Project.toml` (`ShipToolsRepl` stays reachable via `JULIA_LOAD_PATH`, see `spawn_supervisor`);
/// a restart changes it and every later spawn, a death respawn included, uses it. `retiring` is a supervisor whose
/// retirement has not finished: no replacement starts while it is set.
struct Slot {
    project: PathBuf,
    current: Option<Supervisor>,
    retiring: Option<Supervisor>,
}

impl Slot {
    /// Finish retiring what is retiring: the current supervisor, once taken for retirement, or a retained earlier
    /// one. An error leaves a task that has not finished owned; one that finished with an error is reported here
    /// once.
    async fn settle(&mut self) -> Result<()> {
        if self.retiring.is_none() {
            self.retiring = self.current.take();
        }
        let Some(retiring) = self.retiring.as_mut() else {
            return Ok(());
        };
        let outcome = retiring.retire(REPL_RESTART_WAIT).await;
        if retiring.is_done() {
            self.retiring = None;
        }
        outcome
    }
}

struct Submission {
    op: String,
    payload: Value,
    /// `Some` for request/response ops (interrupt, execute): the supervisor
    /// completes it with the terminal res payload. `None` for fire-and-forget
    /// evals: the supervisor does NOT track them in `pending` — completion is
    /// keyed off the streamed `done` frame on the broadcast bus, and the
    /// terminal res ack is dropped.
    reply: Option<oneshot::Sender<Result<Value>>>,
    /// `Some` for `repl.execute`: the supervisor tees every frame for this
    /// submission's eval_id into the collector, loss-free, so the handler can
    /// return the full collected output. `None` for every other op.
    collector: Option<ExecCollector>,
}

impl Repl {
    pub fn new(
        frame_tx: broadcast::Sender<ReplFrameMsg>,
        workspace_id: Option<String>,
        user_project: PathBuf,
        sig: &'static crate::lifecycle::child_signal::Signal,
    ) -> Self {
        Self {
            inner: Arc::new(ReplInner {
                sig,
                slot: Mutex::new(Slot {
                    project: user_project,
                    current: None,
                    retiring: None,
                }),
                frame_tx,
                workspace_id,
                lifecycle: Arc::new(std::sync::Mutex::new(LifecycleCell {
                    gen: 0,
                    state: ReplLifecycle::NotStarted,
                })),
            }),
        }
    }

    /// Current child lifecycle. `NotStarted` until the first eval forces a
    /// spawn; consumed by `workspace.list` to populate `repl_state`.
    pub fn state(&self) -> ReplLifecycle {
        self.inner
            .lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
    }

    /// Where the `ShipToolsRepl` shim project lives, for both deployment
    /// layouts (dev checkout / release install) — ADR 0030 §4. Resolved on
    /// EVERY spawn attempt, never cached on the handle: the field failure this
    /// exists to prevent was a box whose shipped `julia/repl` was missing, so
    /// `resource_dir` fell through to its last-resort compile-time path. With
    /// the answer cached, fixing the install on disk changed nothing until the
    /// daemon was restarted — every later submit re-reported a path that had
    /// stopped being the truth. Same invariant `julia.rs` states for the julia
    /// binary itself ("re-resolved on every spawn attempt, so a removed or
    /// replaced install recovers on the very next attempt").
    fn repl_project() -> PathBuf {
        crate::paths::resource_dir("julia/repl")
    }

    /// Request/response op that NEVER spawns a child: queue the submission on
    /// the live supervisor sender if one exists and await the terminal res
    /// payload, else report `Ok(None)`. Used by `repl.interrupt`, which stays
    /// a simple req→one-res exchange (eval/run_file are fire-and-forget via
    /// `submit`; their frames stream over the broadcast bus instead). The
    /// no-spawn property is the daemon-side guard the #96 CLI pre-flight
    /// approximated from outside: an `ensure_supervisor` here would pay a
    /// full kernel spawn — minutes of precompile — just to answer
    /// `interrupted:false` against a workspace whose REPL was never started
    /// or has died.
    pub async fn request_if_running(&self, op: &str, payload: Value) -> Result<Option<Value>> {
        let tx = {
            let mut slot = self.inner.slot.lock().await;
            if slot.retiring.is_some() {
                slot.settle().await?;
            }
            match slot.current.as_ref() {
                Some(sup) if !sup.tx.is_closed() => sup.tx.clone(),
                _ => return Ok(None),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: Some(reply_tx),
            collector: None,
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("repl supervisor dropped reply channel"))?
            .map(Some)
    }

    /// Execute op (`repl.execute`, ADR 0033): submit WITH both a reply channel
    /// (to capture the shim's authoritative terminal `res`) AND a frame
    /// collector (to gather the run's frames loss-free off the supervisor,
    /// bypassing the lossy broadcast bus). Returns immediately with the reply
    /// receiver and the shared collector; the caller awaits the res under its
    /// own timeout, then reads the collector. Frame ordering guarantees all
    /// frames precede the terminal res on the child's stdout, so by the time
    /// the res arrives the collector holds the complete output.
    pub async fn execute(
        &self,
        op: &str,
        payload: Value,
    ) -> Result<(oneshot::Receiver<Result<Value>>, ExecCollector)> {
        let tx = self.ensure_supervisor().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let collector: ExecCollector = Arc::new(std::sync::Mutex::new(ExecAccum::default()));
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: Some(reply_tx),
            collector: Some(collector.clone()),
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        Ok((reply_rx, collector))
    }

    /// Fire-and-forget op: queue the submission with no reply channel and
    /// return once it's enqueued. The supervisor does NOT insert it into
    /// `pending`; its streamed `repl.frame` evts (including the terminal
    /// `done` frame) are fanned out over the broadcast bus, and the terminal
    /// res ack line is dropped. Used by `repl.eval` / `repl.run_file` so the
    /// connection loop never blocks waiting for an eval to finish (a mid-eval
    /// `repl.interrupt` must still be readable).
    pub async fn submit(&self, op: &str, payload: Value) -> Result<()> {
        let tx = self.ensure_supervisor().await?;
        tx.send(Submission {
            op: op.to_string(),
            payload,
            reply: None,
            collector: None,
        })
        .await
        .map_err(|_| anyhow!("repl supervisor channel closed"))?;
        Ok(())
    }

    async fn ensure_supervisor(&self) -> Result<mpsc::Sender<Submission>> {
        let mut slot = self.inner.slot.lock().await;
        // Liveness is the CHILD's state, not the channel's. `is_closed()` only
        // reports whether the supervisor TASK still holds the receiver, and a
        // task that has already marked itself `Dead` is on its way out — a
        // submission queued into that window would sit in the buffer until the
        // receiver dropped, which is a wait with no child behind it. Ask both,
        // and respawn unless the sender is open AND the child is not dead.
        if slot.retiring.is_none() {
            if let Some(sup) = slot.current.as_ref() {
                if !sup.tx.is_closed() && self.state() != ReplLifecycle::Dead {
                    return Ok(sup.tx.clone());
                }
            }
        }
        // What is there is dead or still retiring: it is retired and joined
        // before the replacement opens, on every route to a spawn.
        slot.settle().await?;
        let sup = self.spawn(&slot.project)?;
        let tx = sup.tx.clone();
        slot.current = Some(sup);
        Ok(tx)
    }

    fn spawn(&self, project: &Path) -> Result<Supervisor> {
        spawn_supervisor(
            project,
            self.inner.frame_tx.clone(),
            self.inner.workspace_id.clone(),
            self.inner.lifecycle.clone(),
            self.inner.sig,
        )
    }

    /// Retire the persistent REPL child and start another with `user_project`
    /// active (`julia --project=<user_project>`). Used by the `r` keybind in
    /// the frontend (priority J): "reset and run" walks up from the file to
    /// find the closest `Project.toml`, calls this, then forwards a plain
    /// `repl.run_file { fresh: false }` to the fresh child.
    ///
    /// The old supervisor is stopped directly, whatever clones of its submit
    /// sender exist and whether or not Julia ever reads its stdin: it closes
    /// out its work, requests the tree's termination and confirms the direct
    /// child reaped, and this method joins it (at most `REPL_RESTART_WAIT`)
    /// before the new child starts. A retirement error or timeout is returned,
    /// the retiring supervisor stays owned and no replacement starts. The
    /// selected project is kept for every later spawn, a death respawn
    /// included.
    pub async fn restart_with_project(&self, user_project: &Path) -> Result<()> {
        let mut slot = self.inner.slot.lock().await;
        slot.settle().await?;
        slot.project = user_project.to_path_buf();
        let sup = self.spawn(user_project)?;
        slot.current = Some(sup);
        Ok(())
    }
}

/// A submit that lands after the child is gone must (re)spawn or fail with the
/// reason — never wait. Field report (v0.6.0-rc.15, a fresh box): the first
/// `repl.execute` came back `repl_died` at 0 ms because the shipped
/// `julia/repl` was missing; after the path was fixed, WITHOUT a daemon
/// restart, the second one logged `repl.execute` and then produced nothing at
/// all — no reply, no child, REPL still `not_started`.
///
/// Unix-only: the stub children are `/bin/sh` scripts, and the argv log they
/// append to is how a test counts spawns and reads back the `--project=` the
/// supervisor actually resolved.
#[cfg(all(test, unix))]
mod respawn_after_death_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use crate::sidecars::contract_tests::pin_env;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh scratch directory, unique per (process, call).
    fn scratch_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!(
            "sot-repl-respawn-{}-{}-{name}",
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A `julia` stand-in that appends its argv to `log` and then runs `body`.
    fn write_stub(path: &Path, log: &Path, body: &str) {
        sot_log::test_exec::write_executable(
            path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$* LP=$JULIA_LOAD_PATH\" >> '{}'\n{body}\n",
                log.display()
            ),
        );
    }

    fn spawn_argv(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A resource root shaped the way `paths::resource_dir` expects, i.e. with
    /// a real `julia/repl` inside it.
    fn resource_root(dir: &Path, name: &str) -> PathBuf {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join("julia").join("repl")).unwrap();
        root
    }

    /// Submit and wait with a deadline the way `handle_repl_execute` does. `Err`
    /// means the submission never settled — the failure this module is about.
    async fn settle(
        repl: &Repl,
        eval_id: u64,
    ) -> std::result::Result<(), tokio::time::error::Elapsed> {
        let (reply_rx, _collector) = repl
            .execute("repl.eval", serde_json::json!({ "eval_id": eval_id }))
            .await
            .expect("submit must not error once a child spawned");
        tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx)
            .await
            .map(|_| ())
    }

    /// A child that dies the instant it is spawned: every later submit gets a
    /// FRESH child (one spawn each), and every one of them settles.
    #[tokio::test]
    async fn submit_after_instant_child_death_respawns() {
        let dir = scratch_dir("instant-death");
        let root = resource_root(&dir, "resources");
        let log = dir.join("argv.log");
        let julia = dir.join("julia");
        write_stub(&julia, &log, "exit 3");
        let _pin = pin_env(&julia, &root);

        let (frame_tx, _frame_rx) = broadcast::channel(64);
        let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), crate::lifecycle::child_signal::process());

        for attempt in 1..=3u64 {
            settle(&repl, attempt)
                .await
                .unwrap_or_else(|_| panic!("attempt {attempt} never settled"));
        }
        assert_eq!(
            spawn_argv(&log).len(),
            3,
            "each submit after a death must spawn its own child"
        );
        assert_eq!(repl.state(), ReplLifecycle::Dead);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launcher that exits leaving a grandchild holding the inherited pipes:
    /// stdout never reaches EOF, so before the `child.wait()` branch the
    /// supervisor outlived its child with a submit channel that still reported
    /// open — and the submission sat there unread, forever.
    #[tokio::test]
    async fn submit_survives_a_launcher_that_leaves_a_grandchild() {
        let dir = scratch_dir("grandchild");
        let root = resource_root(&dir, "resources");
        let log = dir.join("argv.log");
        let julia = dir.join("julia");
        // `exec 3<&0` hands the grandchild a DUP of the stdin pipe (dash
        // otherwise redirects a background command's stdin from /dev/null),
        // so both pipe ends outlive the launcher: writes never break and
        // stdout never reaches EOF. Only the child's own exit says it is gone.
        write_stub(&julia, &log, "exec 3<&0\nsleep 30 &\nexit 0");
        let _pin = pin_env(&julia, &root);

        let (frame_tx, _frame_rx) = broadcast::channel(64);
        let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), crate::lifecycle::child_signal::process());

        settle(&repl, 1)
            .await
            .expect("a launcher's exit must end the supervisor, not strand the submit");
        settle(&repl, 2).await.expect("the retry must settle too");
        assert_eq!(spawn_argv(&log).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spawn that cannot happen at all is reported to the caller AT ONCE, on
    /// every attempt, and never claims a child exists.
    #[tokio::test]
    async fn spawn_failure_is_returned_immediately() {
        let dir = scratch_dir("no-julia");
        let root = resource_root(&dir, "resources");
        let missing = dir.join("julia-that-is-not-there");
        let _pin = pin_env(&missing, &root);

        let (frame_tx, _frame_rx) = broadcast::channel(64);
        let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), crate::lifecycle::child_signal::process());

        for attempt in 1..=2u64 {
            let submitted = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                repl.execute("repl.eval", serde_json::json!({ "eval_id": attempt })),
            )
            .await
            .unwrap_or_else(|_| panic!("attempt {attempt}: a failed spawn must not wait"));
            let err = submitted.err().expect("a failed spawn must be an error");
            assert!(
                format!("{err:#}").contains(&missing.display().to_string()),
                "the error must name what could not be spawned: {err:#}"
            );
            assert_eq!(
                repl.state(),
                ReplLifecycle::NotStarted,
                "a spawn that never happened must not report a child"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The REPL project is resolved on EVERY spawn attempt. This is the field
    /// sequence: a box ships without `julia/repl`, the operator fixes it, and
    /// the very next submit must use the fixed path — the daemon is not
    /// restarted, and a path cached on the handle would keep it broken forever.
    #[tokio::test]
    async fn repl_project_is_resolved_at_every_spawn() {
        let dir = scratch_dir("reresolve");
        let broken = dir.join("broken"); // exists, but holds no julia/repl
        std::fs::create_dir_all(&broken).unwrap();
        let fixed = resource_root(&dir, "fixed");
        let log = dir.join("argv.log");
        let julia = dir.join("julia");
        write_stub(&julia, &log, "exit 3");
        let _pin = pin_env(&julia, &broken);

        let (frame_tx, _frame_rx) = broadcast::channel(64);
        let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), crate::lifecycle::child_signal::process());

        // First attempt: the root has no `julia/repl`, so resolution falls
        // through to this checkout's own copy — NOT the fixed root.
        settle(&repl, 1).await.expect("attempt 1 never settled");
        let first = spawn_argv(&log);
        assert_eq!(first.len(), 1);
        assert!(
            !first[0].contains(&fixed.display().to_string()),
            "the fixed root did not exist yet: {first:?}"
        );

        // The operator fixes the install. No restart, no new handle.
        std::env::set_var("SOT_RESOURCE_ROOT", &fixed);
        settle(&repl, 2).await.expect("attempt 2 never settled");
        let second = spawn_argv(&log);
        assert_eq!(second.len(), 2);
        assert!(
            second[1].contains(&fixed.join("julia").join("repl").display().to_string()),
            "the next spawn must use the fixed path: {second:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard on the new child-exit branch: a child that ANSWERS and then
    /// exits in the same breath must still deliver its answer. The exit branch
    /// is polled last, so the res line it already wrote is routed first — the
    /// caller gets the reply, not "repl terminated".
    #[tokio::test]
    async fn a_child_that_answers_then_exits_still_delivers_its_answer() {
        let dir = scratch_dir("answer-then-exit");
        let root = resource_root(&dir, "resources");
        let log = dir.join("argv.log");
        let julia = dir.join("julia");
        write_stub(
            &julia,
            &log,
            "read -r line || exit 1\n             printf '%s\\n' '{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"repl.eval\",\"payload\":{\"answered\":true}}'\n             exit 0",
        );
        let _pin = pin_env(&julia, &root);

        let (frame_tx, _frame_rx) = broadcast::channel(64);
        let repl = Repl::new(frame_tx, Some("ws".to_string()), dir.clone(), crate::lifecycle::child_signal::process());

        let (reply_rx, _collector) = repl
            .execute("repl.eval", serde_json::json!({ "eval_id": 1 }))
            .await
            .expect("submit");
        let payload = tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx)
            .await
            .expect("the reply must arrive")
            .expect("the supervisor must not drop the reply channel")
            .expect("the res must not be replaced by a death error");
        assert_eq!(payload.get("answered").and_then(Value::as_bool), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
