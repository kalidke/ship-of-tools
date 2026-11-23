use super::*;

/// Minimal `AsyncWrite` that fails every write — simulates the
/// transport socket breaking mid-request without a real socket pair.
struct FailingWriter;

impl AsyncWrite for FailingWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "simulated write failure",
        )))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Round-2 review finding: `pending.insert` used to run AFTER the
/// fallible write, so a write failure stranded the request — the
/// guard's Drop found nothing to flush because the entry was never
/// added. This drives the REAL send path (`send_figure_get`, the same
/// function `run_protocol` calls) against a writer that always fails,
/// then drops the guard exactly as `run_protocol` does on that `?`
/// exit, and asserts the request still reaches `FigureGetFailed` —
/// proving the insert-before-write ordering, not a reimplementation
/// of it.
#[tokio::test]
async fn write_failure_flushes_via_guard_because_insert_precedes_the_write() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host = "test-host".to_string();
    let mut writer = FailingWriter;
    {
        let mut guard = PendingGuard {
            map: HashMap::new(),
            evt_tx: &evt_tx,
            host: host.clone(),
        };
        let result = send_figure_get(
            &mut writer,
            &mut guard,
            42,
            "figures/never-sent.png".to_string(),
            "files:a/b.md".to_string(),
            None,
        )
        .await;
        assert!(result.is_err(), "the simulated write must fail");
        assert!(
            guard.contains_key(&42),
            "the entry must already be in the map when the write fails"
        );
        // `guard` drops here — the same `?` exit `run_protocol` takes.
    }
    let events: Vec<(HostKey, IncomingEvt)> = evt_rx.try_iter().collect();
    assert_eq!(
        events.len(),
        1,
        "the stranded request must flush exactly once, got {events:?}"
    );
    assert!(matches!(
        &events[0],
        (h, IncomingEvt::FigureGetFailed { url }) if h == &host && url == "figures/never-sent.png"
    ));
}

const WORDS: &str = r#"{"page":2,"cut":false,"lines":[[[1,2,3,4,"alpha"]]]}"#;

/// The events a reply frame to the pending words request for page 2 of `files:a.pdf` becomes.
fn words_answer(payload: serde_json::Value, blob: Option<&str>) -> Vec<String> {
    let mut pending = HashMap::from([(1, PendingKind::PageWords { node_id: "files:a.pdf".into(), workspace_id: Some("ws".into()), page: 2 })]);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    handle_response_frame(Frame::res(1, op::PREVIEW_GET, payload), blob.map(|b| b.as_bytes().to_vec()), &mut pending, &evt_tx, &"h".to_string());
    assert!(pending.is_empty(), "the reply consumed its entry");
    evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect()
}

fn said(mime: &str, len: usize) -> serde_json::Value {
    serde_json::json!({ "mime": mime, "blob": { "len": len, "mime": mime } })
}

/// Every reply to a words request is exactly one typed event that names the file, row and page asked, never bytes: the
/// words, the daemon's refusal by its code, and what is no words (a picture, another page, a document the wire's
/// decoder refuses, a reply that does not parse).
#[test]
fn a_words_reply_becomes_one_typed_event() {
    let words = said(sot_protocol::ops::PAGE_WORDS_MIME, WORDS.len());
    let other_page = WORDS.replace("\"page\":2", "\"page\":3");
    let empty_line = r#"{"page":2,"cut":false,"lines":[[]]}"#;
    for (payload, blob, want) in [
        (words.clone(), Some(WORDS), "reply: Words(PageWords { page: 2, cut: false, lines: [[PageWord { left: 1, top: 2, right: 3, bottom: 4, text: \"alpha\" }]] }) }"),
        (serde_json::json!({ "error": "no words", "code": "no_words" }), None, "reply: Refused { code: Some(\"no_words\"), error: \"no words\" } }"),
        (serde_json::json!({ "error": "no words" }), None, "reply: Refused { code: None, error: \"no words\" } }"),
        (said("image/png", 4), Some("\u{89}PNG"), "reply: Refused { code: None, error: \"the answer was image/png, not words\" } }"),
        (words.clone(), Some(other_page.as_str()), "reply: Refused { code: None, error: \"the words are of page 3, not 2\" } }"),
        (said(sot_protocol::ops::PAGE_WORDS_MIME, empty_line.len()), Some(empty_line), "reply: Refused { code: None, error: \"unreadable words: "),
        (words.clone(), None, "reply: Refused { code: None, error: \"unreadable words: "),
        (serde_json::json!({ "unexpected": true }), None, "reply: Refused { code: None, error: \"unreadable reply"),
    ] {
        let shown = words_answer(payload.clone(), blob);
        assert_eq!(shown.len(), 1, "{payload} -> {shown:?}");
        assert!(shown[0].starts_with("PageWords { node_id: \"files:a.pdf\", workspace_id: Some(\"ws\"), page: 2, ") && shown[0].contains(want), "{payload} -> {shown:?}");
    }
}

/// Not replayable: a request waiting when its connection ends yields one `Ended` and is never handed to a later
/// connection, one written and unanswered yields one when the guard drops, and the frame written is `preview.get` for the
/// page with `words` true.
#[tokio::test]
async fn a_words_request_ends_once_when_its_connection_ends_and_is_never_replayed() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let events = |rx: &std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>| -> Vec<String> { rx.try_iter().map(|(_, e)| format!("{e:?}")).collect() };
    let request = || OutgoingReq::PageWords { node_id: "files:a.pdf".into(), workspace_id: None, page: 2 };
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    out_tx.send(request()).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    drop(out_tx);
    let shown = events(&evt_rx);
    assert!(matches!(shown.as_slice(), [e] if e.starts_with("PageWords { node_id: \"files:a.pdf\"") && e.ends_with("reply: Ended }")), "{shown:?}");
    assert!(outbox.recv().await.is_none(), "not handed to a later connection");

    let mut pending = HashMap::new();
    let mut wire = Vec::new();
    send_page_words(&mut wire, &mut pending, 7, "files:a.pdf".into(), None, 2).await.unwrap();
    let frame = sot_protocol::codec::read_frame_blocking(&mut std::io::BufReader::new(&wire[..])).unwrap();
    assert_eq!((frame.op.as_str(), frame.payload), (op::PREVIEW_GET, serde_json::json!({ "node_id": "files:a.pdf", "page": 2, "words": true })));
    drop(PendingGuard { map: pending, evt_tx: &evt_tx, host: "h".into() });
    assert_eq!(events(&evt_rx).len(), 1, "written and unanswered ends once");
}

