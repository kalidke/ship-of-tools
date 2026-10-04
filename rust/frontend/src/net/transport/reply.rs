//! A reply's pending entry, the guard that reports lost figure fetches, and `handle_response_frame`.

use super::*;

/// Internal bookkeeping so a response frame can be deserialized as the right
/// type. Inserted when the writer sends a request; consumed when the matching
/// reply id arrives.
#[derive(Debug)]
pub(super) enum PendingKind {
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
        let urls: Vec<String> = self
            .map
            .drain()
            .filter_map(|(_, kind)| match kind {
                PendingKind::FigureGet { url } => Some(url),
                _ => None,
            })
            .collect();
        for url in urls {
            let _ = self
                .evt_tx
                .send((self.host.clone(), IncomingEvt::FigureGetFailed { url }));
        }
    }
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
        let _ = evt_tx.send((host.clone(), ev));
    };
    if let Some(kind) = pending.remove(&frame.id) {
        match kind {
            PendingKind::TreeChildren { parent_id, workspace_id } => on_tree_children(frame, &emit, parent_id, workspace_id),
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
            PendingKind::ReplEval { eval_id } => on_repl_eval(frame, &emit, eval_id),
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
            PendingKind::ReplRunFile { eval_id, path, fresh } => on_repl_run_file(frame, &emit, eval_id, path, fresh),
            PendingKind::FileDownload { dest, file } => on_file_download(frame, blob, pending, &emit, dest, file),
            PendingKind::FileUpload => on_file_upload(frame, &emit),
            PendingKind::MonitorSubscribe => on_monitor_subscribe(frame, &emit),
            PendingKind::MonitorHistory => on_monitor_history(frame, &emit),
        }
        let _ = blob; // remaining ops carry no blob (download took its own)
        return;
    }
    // Streamed REPL output frame (`repl.frame` evt, ADR 0009 phase-2). One
    // frame, pushed as produced; the consumer appends it to the in-flight
    // scrollback entry live (`Done` is terminal).
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::REPL_FRAME {
        match serde_json::from_value::<ReplFrameEvt>(frame.payload) {
            Ok(ev) => {
                emit(IncomingEvt::ReplFrameStreamed {
                    eval_id: ev.eval_id,
                    workspace_id: ev.workspace_id,
                    frame: ev.frame,
                });
            }
            Err(e) => tracing::warn!(error = %e, "repl.frame evt parse failed"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-seam regression for the round-1 fix: an `{error, code}` reply to
    /// a `figure.get` must produce EXACTLY one `FigureGetFailed`, driven
    /// through the actual `handle_response_frame` dispatcher (not a
    /// reimplementation of its logic) — and must consume the pending entry
    /// so nothing is left to strand.
    #[test]
    fn figure_get_error_envelope_emits_exactly_one_figure_get_failed() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            7,
            PendingKind::FigureGet {
                url: "figures/dead.png".to_string(),
            },
        );
        let frame = Frame::res(
            7,
            op::PREVIEW_GET,
            serde_json::json!({"error": "not_found", "code": "not_found"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "exactly one event for one figure.get error reply, got {events:?}"
        );
        match &events[0] {
            (h, IncomingEvt::FigureGetFailed { url }) => {
                assert_eq!(h, &host);
                assert_eq!(url, "figures/dead.png");
            }
            other => panic!("expected FigureGetFailed, got {other:?}"),
        }
        assert!(
            pending.is_empty(),
            "the reply must consume its pending entry"
        );
    }

    /// A malformed-but-not-explicit-error payload (missing the required
    /// `mime`/`blob` fields without an `error` key either) must ALSO reach
    /// `FigureGetFailed` through the same parse-failure arm — this is what
    /// makes the explicit error-envelope pre-check provably redundant
    /// (deleted per the round-1 simplicity audit).
    #[test]
    fn figure_get_unparseable_reply_without_error_key_still_fails_terminal() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::FigureGet {
                url: "figures/weird.png".to_string(),
            },
        );
        let frame = Frame::res(9, op::PREVIEW_GET, serde_json::json!({"unexpected": true}));
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], (h, IncomingEvt::FigureGetFailed { url }) if h == &host && url == "figures/weird.png")
        );
    }

    /// Version-skew seam: a NEW frontend against an OLD daemon (no
    /// `workspace.activate` support). `OutgoingReq::WorkspaceActivate`
    /// deliberately stamps NO `PendingKind` (see its send-site comment) —
    /// so an old daemon's generic unknown-op reply (`server.rs`'s `other =>`
    /// catch-all: `{"error": "unknown op: workspace.activate"}`, same `op`
    /// and `id` echoed back) finds nothing in `pending` and MUST fall all
    /// the way through to the generic `IncomingEvt::Event` catch-all —
    /// which ui/control/replies.rs's own catch-all (`else { tracing::debug!(%op, "evt"); }`)
    /// only logs at debug and does nothing else. This is what makes the skew
    /// safe: no status-line error, no retry, no effect on the switch that
    /// sent it. A real `WorkspaceActivateRes` success reply lands the exact
    /// same way (also no `PendingKind`), so this one test covers both.
    #[test]
    fn workspace_activate_reply_with_no_pending_falls_through_to_generic_event() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        let frame = Frame::res(
            11,
            op::WORKSPACE_ACTIVATE,
            serde_json::json!({"error": "unknown op: workspace.activate"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1);
        match &events[0] {
            (h, IncomingEvt::Event { op, payload }) => {
                assert_eq!(h, &host);
                assert_eq!(op, op::WORKSPACE_ACTIVATE);
                assert_eq!(
                    payload.get("error").and_then(|v| v.as_str()),
                    Some("unknown op: workspace.activate")
                );
            }
            other => panic!(
                "expected the generic IncomingEvt::Event fallback (no status-line \
                 variant exists for this op), got {other:?}"
            ),
        }
        assert!(
            pending.is_empty(),
            "no PendingKind was ever inserted for workspace.activate — nothing to strand"
        );
    }

    /// Fix 2 (disconnect strands pending forever): dropping the guard —
    /// standing in for `run_protocol` returning through any of its exit
    /// paths — must flush every outstanding `FigureGet` as
    /// `FigureGetFailed`, and must NOT invent events for other pending
    /// kinds (those have no permanent-skip state on the GPU side, so
    /// losing them silently on disconnect is the pre-existing, accepted
    /// behavior).
    #[test]
    fn pending_guard_flushes_figure_gets_on_drop_not_other_kinds() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let host = "test-host".to_string();
        {
            let mut guard = PendingGuard {
                map: HashMap::new(),
                evt_tx: &evt_tx,
                host: host.clone(),
            };
            guard.insert(
                1,
                PendingKind::FigureGet {
                    url: "figures/a.png".to_string(),
                },
            );
            guard.insert(
                2,
                PendingKind::FigureGet {
                    url: "figures/b.png".to_string(),
                },
            );
            guard.insert(3, PendingKind::PtyOpen { target: None });
            // `guard` drops here — the connection-loss scenario.
        }
        let mut urls: Vec<String> = evt_rx
            .try_iter()
            .map(|(h, evt)| {
                assert_eq!(h, host);
                match evt {
                    IncomingEvt::FigureGetFailed { url } => url,
                    other => panic!("only FigureGet entries should flush on drop, got {other:?}"),
                }
            })
            .collect();
        urls.sort();
        assert_eq!(
            urls,
            vec!["figures/a.png".to_string(), "figures/b.png".to_string()]
        );
    }

    /// Real-seam regression, same shape as `figure_get_error_envelope_...`
    /// above: an `attach_direct` refusal to `pty.open`, driven through the
    /// actual `handle_response_frame` dispatcher, must produce exactly one
    /// `PtyAttachDirect` carrying the exact `target` this request's own
    /// `PendingKind::PtyOpen` entry named — L1b fix 1's whole point: the
    /// reply is about THAT row, not whatever the pending map happened to
    /// be keyed against.
    #[test]
    fn attach_direct_reply_emits_exactly_one_pty_attach_direct_with_its_own_target() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::PtyOpen {
                target: Some("sot-be-alpha".to_string()),
            },
        );
        let frame = Frame::res(
            9,
            op::PTY_OPEN,
            serde_json::json!({
                "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
                "code": "attach_direct",
                "state_dir": "/state/workspaces/ws-9",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(
            events.len(),
            1,
            "exactly one event for one attach_direct reply, got {events:?}"
        );
        match &events[0] {
            (h, IncomingEvt::PtyAttachDirect { target }) => {
                assert_eq!(h, &host);
                assert_eq!(target.as_deref(), Some("sot-be-alpha"));
            }
            other => panic!("expected PtyAttachDirect, got {other:?}"),
        }
    }

    #[test]
    fn non_attach_direct_pty_open_reply_emits_pty_open_failed() {
        // This build's daemon never answers `pty.open` with anything but
        // an `attach_direct` refusal (every row is a capsule) — anything
        // else used to be warned-and-dropped, leaving the pane `Pending`
        // forever with buffered keystrokes and no visible explanation.
        // It must now surface as `PtyOpenFailed` so the chrome can show
        // a reason.
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            11,
            PendingKind::PtyOpen {
                target: Some("sot-be-beta".to_string()),
            },
        );
        let frame = Frame::res(11, op::PTY_OPEN, serde_json::json!({"cols": 80, "rows": 24}));
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (h, IncomingEvt::PtyOpenFailed { target, error }) => {
                assert_eq!(h, &host);
                assert_eq!(target.as_deref(), Some("sot-be-beta"));
                assert_eq!(error, "unsupported daemon reply");
            }
            other => panic!("expected PtyOpenFailed, got {other:?}"),
        }
    }

    #[test]
    fn non_attach_direct_pty_open_reply_uses_the_replys_own_code_as_the_reason() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            12,
            PendingKind::PtyOpen {
                target: Some("sot-be-gamma".to_string()),
            },
        );
        let frame = Frame::res(
            12,
            op::PTY_OPEN,
            serde_json::json!({"error": "boom", "code": "bad_target"}),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (_, IncomingEvt::PtyOpenFailed { error, .. }) => assert_eq!(error, "bad_target"),
            other => panic!("expected PtyOpenFailed, got {other:?}"),
        }
    }

    // --- Switch-latency Phase 1: the generation/owner fields the transport
    // threads through `PendingKind` are exactly what the request stamped.
    // The chrome's accept/reject DECISION (`reply_is_current`) lives in
    // ui/preview/fetch.rs and is tested there; this only proves the plumbing.

    #[test]
    fn concept_read_reply_echoes_the_workspace_and_generation_it_was_fired_with() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            21,
            PendingKind::ConceptRead {
                target: "MyModule.myfunction".to_string(),
                workspace_id: Some("ws-a".to_string()),
                generation: 7,
            },
        );
        let frame = Frame::res(
            21,
            op::CONCEPT_READ,
            serde_json::json!({
                "target": "MyModule.myfunction",
                "exists": true,
                "content": "hello",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (
                h,
                IncomingEvt::ConceptRead {
                    target,
                    workspace_id,
                    exists,
                    content,
                    generation,
                },
            ) => {
                assert_eq!(h, &host);
                assert_eq!(target, "MyModule.myfunction");
                assert_eq!(workspace_id.as_deref(), Some("ws-a"));
                assert!(*exists);
                assert_eq!(content, "hello");
                assert_eq!(*generation, 7);
            }
            other => panic!("expected ConceptRead, got {other:?}"),
        }
    }

    #[test]
    fn preview_get_reply_echoes_the_generation_it_was_fired_with() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            22,
            PendingKind::PreviewGet {
                node_id: "files:a.md".to_string(),
                workspace_id: Some("ws-b".to_string()),
                generation: 42,
            },
        );
        let frame = Frame::res(
            22,
            op::PREVIEW_GET,
            serde_json::json!({
                "mime": "text/markdown",
                "blob": {"len": 0, "mime": "text/markdown"},
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, Some(Vec::new()), &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (
                h,
                IncomingEvt::Preview {
                    node_id,
                    workspace_id,
                    generation,
                    ..
                },
            ) => {
                assert_eq!(h, &host);
                assert_eq!(node_id.as_deref(), Some("files:a.md"));
                assert_eq!(workspace_id.as_deref(), Some("ws-b"));
                assert_eq!(*generation, 42);
            }
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    /// A `preview.get` error envelope (as `handle_preview_get` sends for
    /// e.g. `code: "kernel_unavailable"`) must surface as
    /// `PreviewGetFailed`, not silently fail `PreviewGetRes` deserialization
    /// — and must echo the SAME ownership pair the success path does, so a
    /// stale one can be dropped identically.
    #[test]
    fn preview_get_error_envelope_emits_preview_get_failed() {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let mut pending: HashMap<u64, PendingKind> = HashMap::new();
        pending.insert(
            9,
            PendingKind::PreviewGet {
                node_id: "files:data.h5".to_string(),
                workspace_id: Some("ws-a".to_string()),
                generation: 3,
            },
        );
        let frame = Frame::res(
            9,
            op::PREVIEW_GET,
            serde_json::json!({
                "error": "Julia kernel unavailable: julia exited at once",
                "code": "kernel_unavailable",
            }),
        );
        let host = "test-host".to_string();
        handle_response_frame(frame, None, &mut pending, &evt_tx, &host);

        let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
        assert_eq!(events.len(), 1, "got {events:?}");
        match &events[0] {
            (h, IncomingEvt::PreviewGetFailed { node_id, workspace_id, generation, message }) => {
                assert_eq!(h, &host);
                assert_eq!(node_id.as_deref(), Some("files:data.h5"));
                assert_eq!(workspace_id.as_deref(), Some("ws-a"));
                assert_eq!(*generation, 3);
                assert_eq!(message, "kernel_unavailable: Julia kernel unavailable: julia exited at once");
            }
            other => panic!("expected PreviewGetFailed, got {other:?}"),
        }
    }
}
