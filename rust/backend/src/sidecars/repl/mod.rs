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
pub(crate) mod numbers;
pub(crate) mod ops;
mod escalation;
mod supervisor;

#[cfg(test)]
mod interrupt_tests;

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod tree_argv_tests;

use lifecycle::ReplLifecycle;
use lifecycle::{LifecycleCell, SharedLifecycle};
use supervisor::{spawn_supervisor, Supervisor};

/// How long a retirement (the stop, the tree's termination request and the direct child's reap) may take before a
/// restart gives up with an error. It bounds that asynchronous wait only, not lock acquisition or the OS calls.
pub(crate) const REPL_RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long `repl.interrupt` waits for the child's answer once its submission is queued.
pub(crate) const REPL_INTERRUPT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A submission refused because the REPL's queue was full: no caller waits on a REPL that is not taking its input.
/// Its code on the wire is `repl_busy`, and `repl.execute`'s outcome is `busy`.
#[derive(Debug)]
pub(crate) struct ReplBusy;

impl std::fmt::Display for ReplBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the REPL's queue is full")
    }
}

impl std::error::Error for ReplBusy {}

/// Why a REPL supervisor ended with work unfinished: the error each waiting reply gets, the close-out frame each
/// streamed eval gets, and `repl.execute`'s outcome (`restarted` for a restart, else `repl_died`). Only a death is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplEnded {
    /// Its owner retired it to start another (`Repl::restart_with_project`).
    Restarted,
    /// The daemon is shutting down, or the REPL's owner is gone (its row ended).
    Stopped,
    /// The child exited, or its pipes failed.
    Died,
    /// Its own display ended under it (Linux), so the supervisor ended the child: a child left on an ended display
    /// could open the display's name once another account binds it.
    DisplayEnded,
}

impl ReplEnded {
    /// What the next child says at its first eval about this end (`SOT_PREVIOUS_END`), for an end nobody asked for
    /// that takes the REPL's state with it: its display's end.
    fn successor_notice(self) -> Option<&'static str> {
        match self {
            Self::DisplayEnded => Some(
                "the REPL before this one was ended because its own display (Xvfb) ended; this is a new REPL with a \
                 new display, and nothing the old one defined or loaded is here",
            ),
            Self::Restarted | Self::Stopped | Self::Died => None,
        }
    }
}

impl std::fmt::Display for ReplEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Restarted => "the REPL was restarted before this eval ended, so it did not finish",
            Self::Stopped => "the REPL was stopped (its row ended or the daemon is shutting down) before this eval ended",
            Self::Died => {
                "REPL process died mid-eval — it respawns on your next eval; the backend log has its stderr tail"
            }
            Self::DisplayEnded => {
                "the REPL's own display (Xvfb) ended, so the REPL was ended with it before this eval ended; your next \
                 eval starts a new REPL with a new display"
            }
        })
    }
}

impl std::error::Error for ReplEnded {}

/// Queue one submission without waiting: a full queue is [`ReplBusy`].
fn enqueue(tx: &mpsc::Sender<Submission>, sub: Submission) -> Result<()> {
    tx.try_send(sub).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => anyhow::Error::new(ReplBusy),
        mpsc::error::TrySendError::Closed(_) => anyhow!("repl supervisor channel closed"),
    })
}

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
    /// The connection that opens a `browser` frame's page, set once by the first connection that writes the frame
    /// (`server/events.rs`, `repl_frame_for`), so every connection writes it from one answer.
    pub opener: Arc<std::sync::OnceLock<Option<u64>>>,
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
    sig: &'static sot_containment::Signal,
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
    /// The runs of the window that submitted a fire-and-forget run (`numbers::CallerNumbers`): `submit` marks the run
    /// there running once the queue accepts it, and the supervisor marks it ended before it publishes the run's `done`,
    /// so a hello names the window's runs still running. `None` for a request/response op, which is no window's run.
    owner: Option<numbers::CallerNumbers>,
}

impl Repl {
    pub fn new(
        frame_tx: broadcast::Sender<ReplFrameMsg>,
        workspace_id: Option<String>,
        user_project: PathBuf,
        sig: &'static sot_containment::Signal,
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
                    notice: None,
                    start: None,
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

    /// What a caller still waiting on this REPL is told while its child has not said it is ready: how long it has been
    /// starting and what it last wrote to stderr (`Start::still_starting`); `None` once it has, or with no child.
    pub(crate) fn still_starting(&self) -> Option<String> {
        let cell = self.inner.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        match (cell.state, &cell.start) {
            (ReplLifecycle::Starting, Some(start)) => Some(start.still_starting("the REPL")),
            _ => None,
        }
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
        sot_platform::host::paths::resource_dir("julia/repl")
    }

    /// Request/response op that NEVER spawns a child: queue the submission on
    /// the live supervisor sender if one exists and await the terminal res
    /// payload, at most `REPL_INTERRUPT_WAIT`, else report `Ok(None)`. Used by `repl.interrupt`, which stays
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
        enqueue(
            &tx,
            Submission {
                op: op.to_string(),
                payload,
                reply: Some(reply_tx),
                collector: None,
                owner: None,
            },
        )?;
        tokio::time::timeout(REPL_INTERRUPT_WAIT, reply_rx)
            .await
            .map_err(|_| {
                anyhow!(
                    "the REPL did not answer {op} within {REPL_INTERRUPT_WAIT:?}: it was not reading (no eval was running for the \
                     daemon to signal, the interrupt named another, or another interrupt's signal was already in \
                     progress), and a restart ends it now (`sot-fe repl run \
                     <ws> <file> --fresh`, or `r` on a .jl file in the window)"
                )
            })?
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
        enqueue(
            &tx,
            Submission {
                op: op.to_string(),
                payload,
                reply: Some(reply_tx),
                collector: Some(collector.clone()),
                owner: None,
            },
        )?;
        Ok((reply_rx, collector))
    }

    /// Fire-and-forget op: queue the submission with no reply channel and
    /// return once it's enqueued, or at once with [`ReplBusy`] when the queue
    /// is full. The supervisor does NOT insert it into
    /// `pending`; its streamed `repl.frame` evts (including the terminal
    /// `done` frame) are fanned out over the broadcast bus, and the terminal
    /// res ack line is dropped. Used by `repl.eval` / `repl.run_file` so the
    /// connection loop never blocks waiting for an eval to finish (a mid-eval
    /// `repl.interrupt` must still be readable). `owner` holds the submitting
    /// window's runs, where this marks the run running once the queue accepts it (a refusal comes before: a full queue or
    /// a REPL that cannot start, each an `Err` here) and the supervisor marks it ended.
    pub async fn submit(&self, op: &str, payload: Value, owner: &numbers::CallerNumbers) -> Result<()> {
        let tx = self.ensure_supervisor().await?;
        let eid = payload.get("eval_id").and_then(Value::as_u64);
        enqueue(
            &tx,
            Submission {
                op: op.to_string(),
                payload,
                reply: None,
                collector: None,
                owner: Some(owner.clone()),
            },
        )?;
        if let Some(eid) = eid {
            owner.accepted(eid);
        }
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
        let sup = self.spawn(&slot.project).await?;
        let tx = sup.tx.clone();
        slot.current = Some(sup);
        Ok(tx)
    }

    async fn spawn(&self, project: &Path) -> Result<Supervisor> {
        spawn_supervisor(
            project,
            self.inner.frame_tx.clone(),
            self.inner.workspace_id.clone(),
            self.inner.lifecycle.clone(),
            self.inner.sig,
        )
        .await
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
        let sup = self.spawn(user_project).await?;
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
#[path = "respawn_after_death_tests.rs"]
mod respawn_after_death_tests;
