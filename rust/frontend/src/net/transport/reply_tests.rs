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
/// so an old daemon's generic unknown-op reply (`server/dispatch.rs`'s `other =>`
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

struct RefusingWriter;
impl AsyncWrite for RefusingWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::other("refused")))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn attempt(workspace_id: &str, result_serial: u64, attempt_serial: u64) -> ResultAttemptId {
    ResultAttemptId {
        workspace_id: workspace_id.to_string(),
        result_serial,
        attempt_serial,
    }
}

fn tree_node(id: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "label": id, "kind": "file", "has_children": false })
}

fn children(parent: &str) -> OutgoingReq {
    OutgoingReq::ResultTree {
        attempt: attempt("id-a", 1, 1),
        request: ResultTreeRequest::Children {
            parent_id: parent.to_string(),
        },
    }
}

async fn written(
    request_id: u64,
    request: OutgoingReq,
    driver: &mut ResultTreeTestDriver<'_>,
) -> Frame {
    let mut wire = Vec::new();
    driver.send(&mut wire, request_id, request).await.unwrap();
    codec::read_frame(&mut codec::buffered(&wire[..]))
        .await
        .unwrap()
        .0
}

/// Two requests for the same row and parent differ only in the attempt that issued them: each
/// reply carries back the tag saved for its own request id, whatever order the replies arrive in.
#[tokio::test]
async fn result_tree_replies_preserve_the_issuing_attempt() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host = "test-host".to_string();
    let mut driver = ResultTreeTestDriver::new(host.clone(), &evt_tx);
    let first = written(7, children("files:src"), &mut driver).await;
    let second = written(
        8,
        OutgoingReq::ResultTree {
            attempt: attempt("id-a", 2, 2),
            request: ResultTreeRequest::Children {
                parent_id: "files:src".to_string(),
            },
        },
        &mut driver,
    )
    .await;
    let ordinary = written(
        9,
        OutgoingReq::TreeChildren {
            parent_id: "files:src".to_string(),
            workspace_id: Some("id-a".to_string()),
        },
        &mut driver,
    )
    .await;
    for frame in [&first, &second] {
        assert_eq!(frame.op, op::TREE_CHILDREN);
        assert_eq!(
            frame.payload, ordinary.payload,
            "a result request is the ordinary tree request on the wire"
        );
    }
    // The same payload answers both, in the reverse order.
    let reply = |id: u64| {
        Frame::res(
            id,
            op::TREE_CHILDREN,
            serde_json::json!({ "children": [tree_node("files:src/a.rs")] }),
        )
    };
    driver.reply(reply(8));
    driver.reply(reply(7));
    driver.reply(reply(8));
    let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
    let tagged: Vec<(&ResultAttemptId, &ResultTreeReply)> = events
        .iter()
        .filter_map(|(_, evt)| match evt {
            IncomingEvt::ResultTree { attempt, reply } => Some((attempt, reply)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tagged.len(),
        2,
        "one terminal tagged event per pending result request: {events:?}"
    );
    assert_eq!(*tagged[0].0, attempt("id-a", 2, 2));
    assert_eq!(*tagged[1].0, attempt("id-a", 1, 1));
    for (_, reply) in &tagged {
        assert!(
            matches!(reply, ResultTreeReply::Children { parent_id, children }
            if parent_id == "files:src" && children.len() == 1)
        );
    }
    assert!(
        !events
            .iter()
            .any(|(_, evt)| matches!(evt, IncomingEvt::TreeChildren { .. })),
        "a repeated response id became a tree installation: {events:?}"
    );
}

/// Backend errors and malformed replies fail the saved request, root and children alike.
#[tokio::test]
async fn result_tree_failures_return_the_saved_tag() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host = "test-host".to_string();
    let mut driver = ResultTreeTestDriver::new(host.clone(), &evt_tx);
    let root = OutgoingReq::ResultTree {
        attempt: attempt("id-a", 3, 3),
        request: ResultTreeRequest::Root,
    };
    written(10, root, &mut driver).await;
    written(11, children("files:lib"), &mut driver).await;
    written(
        12,
        OutgoingReq::ResultTree {
            attempt: attempt("id-a", 3, 4),
            request: ResultTreeRequest::Root,
        },
        &mut driver,
    )
    .await;
    driver.reply(Frame::res(
        10,
        op::TREE_ROOT,
        serde_json::json!({ "error": "boom", "code": "x" }),
    ));
    driver.reply(Frame::res(
        11,
        op::TREE_CHILDREN,
        serde_json::json!({ "nonsense": 1 }),
    ));
    driver.reply(Frame::res(
        12,
        op::TREE_ROOT,
        serde_json::json!({ "nonsense": 1 }),
    ));
    let failures: Vec<(ResultAttemptId, ResultTreeRequest, String)> = evt_rx
        .try_iter()
        .filter_map(|(_, evt)| match evt {
            IncomingEvt::ResultTree {
                attempt,
                reply: ResultTreeReply::Failed { request, error },
            } => Some((attempt, request, error)),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 3);
    assert_eq!(failures[0].0, attempt("id-a", 3, 3));
    assert_eq!(failures[0].1, ResultTreeRequest::Root);
    assert_eq!(failures[0].2, "boom");
    assert_eq!(failures[1].0, attempt("id-a", 1, 1));
    assert_eq!(
        failures[1].1,
        ResultTreeRequest::Children {
            parent_id: "files:lib".to_string()
        }
    );
    assert_eq!(failures[2].0, attempt("id-a", 3, 4));
    assert_eq!(failures[2].1, ResultTreeRequest::Root);
}

/// A refused write and a connection that ends both fail what is still pending, once each.
#[tokio::test]
async fn result_tree_loss_fails_pending_requests_once() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host = "test-host".to_string();
    let mut driver = ResultTreeTestDriver::new(host.clone(), &evt_tx);
    let mut refusing = RefusingWriter;
    assert!(driver
        .send(
            &mut refusing,
            13,
            OutgoingReq::ResultTree {
                attempt: attempt("id-a", 4, 5),
                request: ResultTreeRequest::Root
            }
        )
        .await
        .is_err());
    written(14, children("files:docs"), &mut driver).await;
    drop(driver);
    let mut lost: Vec<ResultAttemptId> = evt_rx
        .try_iter()
        .filter_map(|(_, evt)| match evt {
            IncomingEvt::ResultTree {
                attempt,
                reply: ResultTreeReply::Failed { .. },
            } => Some(attempt),
            _ => None,
        })
        .collect();
    lost.sort_by_key(|a| a.result_serial);
    assert_eq!(lost, vec![attempt("id-a", 1, 1), attempt("id-a", 4, 5)]);
}
