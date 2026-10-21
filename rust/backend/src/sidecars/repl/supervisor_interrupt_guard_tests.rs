use super::*;

/// `request_if_running` must be a no-spawn path: with no supervisor ever
/// started it reports `Ok(None)` and leaves the lifecycle at `NotStarted`
/// — the daemon-side twin of the #96 CLI guard, where `request`'s
/// `ensure_supervisor` would boot a kernel (minutes of precompile) just
/// to answer `interrupted:false`.
#[tokio::test]
async fn request_if_running_never_spawns() {
    let (tx, _rx) = broadcast::channel(8);
    let repl = Repl::new(tx, None, std::env::temp_dir(), crate::lifecycle::child_signal::process());
    let res = repl
        .request_if_running("repl.interrupt", serde_json::json!({}))
        .await
        .expect("no-child path must not error");
    assert!(res.is_none());
    assert_eq!(repl.state(), ReplLifecycle::NotStarted);
}

/// The boot sentinel is acknowledged and dropped by `route_line` — it
/// must not be misread as a res ack (its id is 0) nor fanned onto the
/// frame bus.
#[test]
fn route_line_drops_ready_sentinel() {
    let (frame_tx, mut frame_rx) = broadcast::channel(8);
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
    let (reply_tx, mut reply_rx) = oneshot::channel();
    pending.insert(0u64, reply_tx); // adversarial: a pending entry at the sentinel's id
    let mut streaming = HashMap::new();
    let mut collectors = HashMap::new();
    let mut collector_ids = HashMap::new();
    route_line(
        r#"{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{"julia":"1.12.5","protocol":1}}"#,
        &mut pending,
        &mut streaming,
        &mut collectors,
        &mut collector_ids,
        &frame_tx,
        &None,
    );
    assert!(
        pending.contains_key(&0),
        "sentinel must not consume a pending reply"
    );
    assert!(reply_rx.try_recv().is_err());
    assert!(
        frame_rx.try_recv().is_err(),
        "sentinel is not a repl.frame; nothing goes on the bus"
    );
}

/// ADR 0049, User isolation: a REPL line that does not parse, or a frame with no eval id, is logged by its length
/// and where it failed, never by its bytes: an announcement cut off inside a `wglshow` token leaves the token's
/// first characters in the next line the reader sees, which no 32-character mask can recognise.
#[test]
fn a_line_that_does_not_parse_is_logged_without_its_bytes() {
    let logged = crate::sidecars::logged_by(|| {
        let (frame_tx, _frame_rx) = broadcast::channel(8);
        let mut pending: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
        let mut streaming = HashMap::new();
        let mut collectors = HashMap::new();
        let mut collector_ids = HashMap::new();
        for line in [
            // An announcement cut off inside the token, with the next envelope appended.
            r#"{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"eval_id":3,"frame":{"kind":"browser","url":"http://127.0.0.1:41234/0123456789ab{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{}}"#,
            // A well-formed frame with no eval id.
            r#"{"v":1,"id":0,"kind":"evt","op":"repl.frame","payload":{"frame":{"kind":"browser","url":"http://127.0.0.1:41234/0123456789abcdef0123456789abcdef"}}}"#,
        ] {
            route_line(line, &mut pending, &mut streaming, &mut collectors, &mut collector_ids, &frame_tx, &None);
        }
    });
    assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 2, "{logged}");
    assert!(!logged.contains("0123"), "{logged}");
}
