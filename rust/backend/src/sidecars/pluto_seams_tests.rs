//! Private test leaves at the real awaits of the supervisor: a count of the polls on which a stdin write was
//! still pending, and a gate every closeout waits on before the checked termination.
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

pub(crate) static WRITE_PENDING_POLLS: AtomicU64 = AtomicU64::new(0);
/// Held (zero permits) while a test wants the flush await to stay pending; one-shot error for the flush.
pub(crate) static FLUSH_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);
pub(crate) static FAIL_FLUSH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) static CLEANUP_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);

/// The leaf in front of the real flush: waits on the gate, then fails once if asked.
pub(crate) async fn before_flush() -> std::io::Result<()> {
    let gate = FLUSH_GATE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(gate) = gate {
        let _ = gate.acquire().await;
    }
    if FAIL_FLUSH.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other("injected flush error"));
    }
    Ok(())
}

pub(crate) async fn observed<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        let polled = future.as_mut().poll(cx);
        if polled.is_pending() {
            WRITE_PENDING_POLLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        polled
    })
    .await
}
