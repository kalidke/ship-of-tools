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
