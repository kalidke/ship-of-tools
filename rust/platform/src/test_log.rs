//! Test-only (feature `test-support`): `capture()` reads this thread's tracing output without formatter timestamps or
//! ANSI colour. `install()` puts a supplied subscriber under test with its formatting intact.
//! A global router, installed once, keeps tracing's process-wide callsite cache from caching `never` for a callsite
//! whose first reach is a thread with no subscriber.

use std::io::Write;
use std::sync::{Arc, Mutex, Once};

use tracing::level_filters::LevelFilter;
use tracing::subscriber::{DefaultGuard, Interest};
use tracing::{span, Event, Metadata, Subscriber};

/// Enables nothing and answers `sometimes` for every callsite, so no callsite is cached `never` and each event asks
/// the dispatcher of the thread that emits it. Its `OFF` hint keeps the level gate closed until a capture opens it.
struct Router;

impl Subscriber for Router {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::OFF)
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

static ROUTER: Once = Once::new();

/// This thread's events at every level while this capture is its default subscriber, without formatter timestamps or colour.
pub struct Capture {
    _guard: DefaultGuard,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// Every line captured so far, without formatter timestamps or colour; event messages and fields are preserved.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs `subscriber` as this thread's default until the guard drops, preserving its formatting and timer.
/// Installs the router first (see `capture`); use this for a test of a production log writer's own output.
pub fn install(subscriber: impl Subscriber + Send + Sync + 'static) -> DefaultGuard {
    ROUTER.call_once(|| {
        tracing::subscriber::set_global_default(Router).expect("a global tracing dispatcher was set before test_log's router");
    });
    tracing::subscriber::set_default(subscriber)
}

/// Starts capturing this thread's events at every level without formatter timestamps or colour.
/// The first `capture` or `install` in a process installs the router as the global dispatcher;
/// it panics if a global dispatcher was set before.
pub fn capture() -> Capture {
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Sink(sink.clone()))
        .with_ansi(false)
        .without_time()
        .with_max_level(LevelFilter::TRACE)
        .finish();
    Capture { _guard: install(subscriber), out }
}

#[cfg(test)]
#[path = "test_log_tests.rs"]
mod tests;
