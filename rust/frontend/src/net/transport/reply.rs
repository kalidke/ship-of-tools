//! A reply's pending entry, the guard that reports lost figure fetches, and `handle_response_frame`.

use super::*;

/// Internal bookkeeping so a response frame can be deserialized as the right
/// type. Inserted when the writer sends a request; consumed when the matching
/// reply id arrives.
#[derive(Debug)]
pub(super) enum PendingKind {
    ResultTree {
        attempt: ResultAttemptId,
        request: ResultTreeRequest,
    },
    TreeChildren {
        parent_id: String,
        workspace_id: Option<String>,
    },
    TreeRoot {
        workspace_id: Option<String>,
    },
    ProjectScan {
        workspace_id: Option<String>,
        generation: u64,
    },
    MarkdownTokenize {
        lang: String,
        source_hash: u64,
    },
    ConceptRead {
        target: String,
        /// Switch-latency Phase 1: the workspace this read was fired for,
        /// plus the request generation — both threaded straight through
        /// to `IncomingEvt::ConceptRead` unchanged. See that variant.
        workspace_id: Option<String>,
        generation: u64,
    },
    ConceptWrite {
        target: String,
    },
    FileRead {
        node_id: String,
    },
    FileWrite {
        node_id: String,
    },
    FileDelete {
        node_id: String,
    },
    DirCreate {
        node_id: String,
    },
    MathRender {
        latex: String,
        display: bool,
    },
    ImageCrop {
        node_id: String,
    },
    FileParse {
        path: String,
        workspace_id: Option<String>,
    },
    FunctionMethods {
        module: String,
        name: String,
        workspace_id: Option<String>,
    },
    PreviewGet {
        node_id: String,
        workspace_id: Option<String>,
        /// Switch-latency Phase 1: preview-slot request generation, threaded
        /// straight through to `IncomingEvt::Preview`. See that variant.
        generation: u64,
    },
    /// Reply to `preview.set_scale`. Carries the SAME `PreviewGetRes` envelope
    /// as a normal preview, so it decodes with the existing type and surfaces
    /// as `IncomingEvt::Preview` — the chrome installs it through the one
    /// preview path it already has (ADR 0034 §5).
    SetScale {
        node_id: String,
        workspace_id: Option<String>,
        /// Same preview-slot generation as `PreviewGet` — set_scale and an
        /// ordinary preview.get share one consumer slot.
        generation: u64,
    },
    FigureGet {
        url: String,
    },
    ReplEval {
        eval_id: u64,
    },
    /// An eval the daemon accepted or announced (`repl.frame` `Started`), or one of the window's that the hello named
    /// still running (`HelloRes::repl_running`), and has not yet ended with its `done` frame, kept under the key
    /// `RUNNING_EVAL | eval_id` (`eval_running`) so `PendingGuard`'s `Drop` can end it when the connection does. No
    /// reply carries this key; only the frames of the eval's own stream touch it.
    EvalRunning {
        eval_id: u64,
    },
    /// ADR 0042 slice L1b fix 1: carries the `target` THIS request
    /// named (mirrors `PtyOpenReq.target`) — the reply (an ordinary
    /// size confirmation, or an `attach_direct` refusal) is always about
    /// THIS target, never whatever row happens to be selected by the
    /// time the reply lands.
    PtyOpen {
        target: Option<String>,
    },
    DirectoryList,
    WorkspaceCreate,
    WorkspaceList,
    AccountsList,
    WorkspaceDestroy,
    PlutoOpen,
    VideoOpen,
    DocsOpen,
    QuartoOpen,
    ReplRunFile {
        eval_id: u64,
        path: String,
        fresh: bool,
    },
    /// A `file.download` is streaming. The pending entry is re-inserted on
    /// each non-`eof` chunk (one request id, many response frames). `file` is
    /// lazily created on the first chunk so a backend error before any chunk
    /// leaves no empty file behind.
    FileDownload {
        dest: PathBuf,
        file: Option<std::fs::File>,
    },
    /// A `file.upload` chunk awaiting its ack. 1:1 — each chunk is its own
    /// request id, so no re-insert.
    FileUpload,
    /// A `monitor.subscribe` awaiting its cadence + roster reply (ADR 0020).
    MonitorSubscribe,
    /// A `monitor.history` awaiting its windowed per-host series (ADR 0020).
    MonitorHistory,
}

/// The key bit of a running eval's pending entry. Request ids count up from 1 per connection (`take_id`) and never
/// reach it.
const RUNNING_EVAL: u64 = 1 << 63;

/// Keeps `eval_id` as a run this connection waits on (`PendingKind::EvalRunning`): its `done` frame ends the wait, and
/// so does the connection's end, with one error frame (`PendingGuard`).
pub(super) fn eval_running(pending: &mut HashMap<u64, PendingKind>, eval_id: u64) {
    pending.insert(RUNNING_EVAL | eval_id, PendingKind::EvalRunning { eval_id });
}

/// RAII wrapper around `run_protocol`'s per-connection reply-correlation
/// map. `run_protocol` can exit through many paths — a bad read/write via
/// `?`, hello rejection, the outer reconnect loop tearing the task down —
/// and every one of them used to just drop the map in place, silently
/// discarding whatever requests were still in flight. For most
/// `PendingKind`s that's harmless (the UI re-fires on the next user
/// action), but a lost `FigureGet` has no such recovery: the GPU side's
/// `figure_pending` has no way to learn the reply is never coming, so
/// `dispatch_pending_figures` treats the url as "still in flight" forever
/// (field report round 2). `Drop` is the one place that runs on every exit
/// path without needing to touch each of them, so it carries the
/// invariant here: every fired `figure.get` terminates in exactly one of
/// `FigureLoaded` / `FigureGetFailed`, connection loss included.
///
/// `Deref`/`DerefMut` to the inner map so every existing `pending.insert`
/// / `&mut pending` call site in `run_protocol` needs no change.
pub(super) struct PendingGuard<'a> {
    pub(super) map: HashMap<u64, PendingKind>,
    pub(super) evt_tx: &'a StdSender<(HostKey, IncomingEvt)>,
    /// ADR 0042 L2a: which connection this guard belongs to, so its Drop
    /// tags the `FigureGetFailed` flush the same way every other send on
    /// this connection is tagged.
    pub(super) host: HostKey,
}

impl std::ops::Deref for PendingGuard<'_> {
    type Target = HashMap<u64, PendingKind>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl std::ops::DerefMut for PendingGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        for (_, kind) in self.map.drain() {
            let event = match kind {
                PendingKind::FigureGet { url } => IncomingEvt::FigureGetFailed { url },
                PendingKind::ResultTree { attempt, request } => IncomingEvt::ResultTree {
                    attempt,
                    reply: ResultTreeReply::Failed {
                        request,
                        error: "connection ended before result tree completion".into(),
                    },
                },
                PendingKind::MathRender { latex, display } => IncomingEvt::MathRenderFailed {
                    latex,
                    display,
                    message: "the connection ended before the render's reply".into(),
                },
                // Exactly what an error reply already produces: no spans, so the block keeps its base highlighting.
                PendingKind::MarkdownTokenize { lang, source_hash } => {
                    IncomingEvt::MarkdownTokens { lang, source_hash, spans: Vec::new() }
                }
                // An eval or `r`/`R` run whose ack had not come is ended too: its request may or may not have reached
                // the daemon, and if its REPL accepted it before the next hello and the run still goes on, that hello
                // names it (a REPL start that outlasts the redial accepts it later: the daemon's repl page).
                PendingKind::ReplEval { eval_id }
                | PendingKind::ReplRunFile { eval_id, .. }
                | PendingKind::EvalRunning { eval_id } => IncomingEvt::ReplEvalDone {
                    eval_id,
                    elapsed_ms: 0,
                    frames: vec![ReplFrame::Error {
                        message: "the connection to the daemon ended before this eval's end".into(),
                        stacktrace: Vec::new(),
                    }],
                },
                _ => continue,
            };
            let _ = self.evt_tx.send((self.host.clone(), event));
        }
    }
}

#[cfg(all(test, feature = "test-window-progress"))]
static NATIVE_OBSERVER: std::sync::Mutex<
    Option<std::sync::Arc<std::sync::Mutex<crate::ui::NativeProgressLedger>>>,
> = std::sync::Mutex::new(None);

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) struct NativeFanInObservation(
    Option<std::sync::Arc<std::sync::Mutex<crate::ui::NativeProgressLedger>>>,
);

#[cfg(all(test, feature = "test-window-progress"))]
impl Drop for NativeFanInObservation {
    fn drop(&mut self) {
        *NATIVE_OBSERVER.lock().expect("native observer") = self.0.take();
    }
}

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) fn observe_native_fan_in(
    ledger: std::sync::Arc<std::sync::Mutex<crate::ui::NativeProgressLedger>>,
) -> NativeFanInObservation {
    NativeFanInObservation(
        NATIVE_OBSERVER
            .lock()
            .expect("native observer")
            .replace(ledger),
    )
}

/// Route a frame to the right `IncomingEvt`. Replies look up `id` in the
/// pending map to decide how to deserialize; everything else falls through
/// to the catch-all `Event` evt so the GPU thread can at least see it.
pub(super) fn handle_response_frame(
    frame: Frame,
    blob: Option<Vec<u8>>,
    pending: &mut HashMap<u64, PendingKind>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    host: &HostKey,
) {
    // Same tag-at-the-send pattern as `run_protocol`'s own `emit` — this
    // function carries essentially all of a connection's inbound sends, so
    // it needs its own copy rather than threading `run_protocol`'s closure
    // across a function boundary.
    let emit = |ev: IncomingEvt| {
        #[cfg(all(test, feature = "test-window-progress"))]
        {
            let ledger = NATIVE_OBSERVER.lock().expect("native observer").clone();
            if let Some(ledger) = ledger {
                let mut ledger = ledger.lock().expect("native progress ledger");
                let tag = ledger.tag(host, &ev);
                if evt_tx.send((host.clone(), ev)).is_err() {
                    // The proof's fan-in must outlive its transport. A send that fails is a broken proof, named at
                    // the first failed send rather than read later as no event sent.
                    drop(ledger);
                    panic!("native fan-in: the fan-in receiver is gone");
                }
                ledger.accepted(tag);
                return;
            }
        }
        let _ = evt_tx.send((host.clone(), ev));
    };
    if let Some(kind) = pending.remove(&frame.id) {
        match kind {
            PendingKind::ResultTree { attempt, request } => {
                on_result_tree(frame, &emit, attempt, request)
            }
            PendingKind::TreeChildren {
                parent_id,
                workspace_id,
            } => on_tree_children(frame, &emit, parent_id, workspace_id),
            PendingKind::TreeRoot { workspace_id } => on_tree_root(frame, &emit, workspace_id),
            PendingKind::ProjectScan { workspace_id, generation } => on_project_scan(frame, &emit, workspace_id, generation),
            PendingKind::MarkdownTokenize { lang, source_hash } => on_markdown_tokenize(frame, &emit, lang, source_hash),
            PendingKind::ConceptRead { target, workspace_id, generation } => on_concept_read(frame, &emit, target, workspace_id, generation),
            PendingKind::MathRender { latex, display } => on_math_render(frame, blob, &emit, latex, display),
            PendingKind::ImageCrop { node_id } => on_image_crop(frame, &emit, node_id),
            PendingKind::ConceptWrite { target } => on_concept_write(frame, &emit, target),
            PendingKind::FileRead { node_id } => on_file_read(frame, &emit, node_id),
            PendingKind::FileWrite { node_id } => on_file_write(frame, &emit, node_id),
            PendingKind::FileDelete { node_id } => on_file_delete(frame, &emit, node_id),
            PendingKind::DirCreate { node_id } => on_dir_create(frame, &emit, node_id),
            PendingKind::FileParse { path, workspace_id } => on_file_parse(frame, &emit, path, workspace_id),
            PendingKind::PreviewGet { node_id, workspace_id, generation } => on_preview_get(frame, blob, &emit, node_id, workspace_id, generation),
            PendingKind::SetScale { node_id, workspace_id, generation } => on_set_scale(frame, blob, &emit, node_id, workspace_id, generation),
            PendingKind::FigureGet { url } => on_figure_get(frame, blob, &emit, url),
            PendingKind::FunctionMethods { module, name, workspace_id } => on_function_methods(frame, &emit, module, name, workspace_id),
            PendingKind::ReplEval { eval_id } => {
                if frame.payload.get("accepted").and_then(|v| v.as_bool()) == Some(true) {
                    eval_running(pending, eval_id);
                }
                on_repl_eval(frame, &emit, eval_id)
            }
            // Only the eval's own stream touches this entry, never a reply id (`RUNNING_EVAL`).
            PendingKind::EvalRunning { .. } => {}
            PendingKind::PtyOpen { target } => on_pty_open(frame, &emit, target),
            PendingKind::DirectoryList => on_directory_list(frame, &emit),
            PendingKind::WorkspaceCreate => on_workspace_create(frame, &emit),
            PendingKind::WorkspaceList => on_workspace_list(frame, &emit),
            PendingKind::AccountsList => on_accounts_list(frame, &emit),
            PendingKind::WorkspaceDestroy => on_workspace_destroy(frame, &emit),
            PendingKind::PlutoOpen => on_pluto_open(frame, &emit),
            PendingKind::VideoOpen => on_video_open(frame, &emit),
            PendingKind::DocsOpen => on_docs_open(frame, &emit),
            PendingKind::QuartoOpen => on_quarto_open(frame, blob, &emit),
            PendingKind::ReplRunFile { eval_id, path, fresh } => {
                if frame.payload.get("frames").is_some() {
                    eval_running(pending, eval_id);
                }
                on_repl_run_file(frame, &emit, eval_id, path, fresh)
            }
            PendingKind::FileDownload { dest, file } => on_file_download(frame, blob, pending, &emit, dest, file),
            PendingKind::FileUpload => on_file_upload(frame, &emit),
            PendingKind::MonitorSubscribe => on_monitor_subscribe(frame, &emit),
            PendingKind::MonitorHistory => on_monitor_history(frame, &emit),
        }
        let _ = blob; // remaining ops carry no blob (download took its own)
        return;
    }
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::REPL_FRAME {
        on_repl_frame(frame, pending, &emit);
        return;
    }
    // Live server-metrics tick (`monitor.tick` evt, ADR 0020). One sample per
    // host at the subscribed cadence; the consumer appends each to its ring.
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::MONITOR_TICK {
        match serde_json::from_value::<MonitorTickEvt>(frame.payload) {
            Ok(ev) => {
                emit(IncomingEvt::MonitorTick { hosts: ev.hosts });
            }
            Err(e) => tracing::warn!(error = %e, "monitor.tick evt parse failed"),
        }
        return;
    }
    emit(IncomingEvt::Event {
        op: frame.op,
        payload: frame.payload,
    });
    if blob.is_some() {
        // Spike handlers don't emit unsolicited blobs; drop and move on.
    }
}

/// A streamed REPL output frame (`repl.frame` evt, ADR 0009 phase-2). One frame, pushed as produced; the consumer
/// appends it to the in-flight scrollback entry live (`Done` is terminal). The stream also keeps the eval's pending
/// entry: `Started` opens it and `Done` closes it (`PendingKind::EvalRunning`).
fn on_repl_frame(frame: Frame, pending: &mut HashMap<u64, PendingKind>, emit: &impl Fn(IncomingEvt)) {
    match serde_json::from_value::<ReplFrameEvt>(frame.payload) {
        Ok(ev) => {
            match &ev.frame {
                ReplFrame::Started { .. } => eval_running(pending, ev.eval_id),
                ReplFrame::Done { .. } => {
                    pending.remove(&(RUNNING_EVAL | ev.eval_id));
                }
                _ => {}
            }
            emit(IncomingEvt::ReplFrameStreamed { eval_id: ev.eval_id, workspace_id: ev.workspace_id, frame: ev.frame });
        }
        Err(e) => tracing::warn!(error = %e, "repl.frame evt parse failed"),
    }
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reply_wait_end_tests.rs"]
mod wait_end_tests;
