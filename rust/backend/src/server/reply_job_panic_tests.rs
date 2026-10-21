use super::*;

fn boom() -> Result<HandlerOutput> {
    panic!("probe panic")
}

/// A handler that panics is answered with the standard error reply naming the panic, not left to its waiter.
#[tokio::test]
async fn a_panicking_job_is_answered() {
    let (out_tx, mut out_rx) = mpsc::channel(4);
    let mut jobs: JoinSet<()> = JoinSet::new();
    spawn_job(&mut jobs, Some(Arc::new(Semaphore::new(1))), out_tx, 7, "probe.op".to_string(), async { boom() });
    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), out_rx.recv())
        .await
        .expect("a panicked job sent no reply within 2s")
        .expect("the reply channel closed");
    println!("job-panic: reply id {} payload {}", reply.0.id, reply.0.payload);
    assert_eq!(reply.0.id, 7);
    assert!(
        reply.0.payload["error"].as_str().is_some_and(|e| e.contains("probe.op panicked: probe panic")),
        "{}",
        reply.0.payload
    );
}
