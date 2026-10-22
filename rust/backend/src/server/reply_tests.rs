use super::*;

/// A pooled job that waits `OFFLOOP_QUEUE_TIMEOUT` for a permit is answered with the timeout and its handler never
/// runs. Virtual time: the semaphore has no permit and nothing releases one.
#[tokio::test(start_paused = true)]
async fn a_pooled_job_that_waits_ten_seconds_for_a_permit_is_answered_and_never_runs() {
    let (out_tx, mut out_rx) = mpsc::channel(4);
    let mut jobs: JoinSet<()> = JoinSet::new();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler_ran = ran.clone();
    spawn_job(&mut jobs, Some(Arc::new(Semaphore::new(0))), out_tx, 9, "probe.op".to_string(), async move {
        handler_ran.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Vec::new())
    });
    let started = tokio::time::Instant::now();
    let reply = out_rx.recv().await.expect("a job that waited for a permit sent no reply");
    assert_eq!(reply.0.id, 9);
    assert!(
        reply.0.payload["error"].as_str().is_some_and(|e| e.contains("probe.op timed out after") && e.contains("off-loop queue saturated")),
        "{}",
        reply.0.payload
    );
    assert!(started.elapsed() >= OFFLOOP_QUEUE_TIMEOUT, "answered before the bound: {:?}", started.elapsed());
    while jobs.join_next().await.is_some() {}
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst), "the handler ran without a permit");
}
