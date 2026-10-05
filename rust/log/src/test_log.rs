//! Test-only (feature `test-support`): every subscriber a test installs goes through here: `capture()` reads a thread's tracing output, and `install()` puts a production log writer's own subscriber under test.
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

/// Every event of the thread that called [`capture`], at every level, while the value lives.
pub struct Capture {
    _guard: DefaultGuard,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// Every line captured so far, as `tracing_subscriber::fmt` writes them without colour.
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

/// Installs `subscriber` as this thread's default until the guard drops, after the router (see `capture`). For a test of
/// a production log writer, which must see that writer's own output.
pub fn install(subscriber: impl Subscriber + Send + Sync + 'static) -> DefaultGuard {
    ROUTER.call_once(|| {
        tracing::subscriber::set_global_default(Router).expect("a global tracing dispatcher was set before test_log's router");
    });
    tracing::subscriber::set_default(subscriber)
}

/// Starts capturing this thread's events at every level. The first `capture` or `install` in a process installs the
/// router as the global dispatcher; it panics if a global dispatcher was set before.
pub fn capture() -> Capture {
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Sink(sink.clone()))
        .with_ansi(false)
        .with_max_level(LevelFilter::TRACE)
        .finish();
    Capture { _guard: install(subscriber), out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() {
        tracing::info!("test_log probe");
    }

    /// The callsite is reached first by a thread with no subscriber; the capture still sees the later event.
    #[test]
    fn a_callsite_another_thread_reaches_first_still_reaches_the_capture() {
        let log = capture();
        std::thread::spawn(probe).join().unwrap();
        probe();
        assert_eq!(log.text().matches("test_log probe").count(), 1, "{}", log.text());
    }

    /// The whole record of where a subscriber is made or installed: this module; each binary's `main` by its production
    /// lines; and the subscriber the log-writer test in secret.rs builds, by exact line and count. Every installation goes
    /// through `install` or `capture`.
    #[test]
    fn only_test_log_makes_a_subscriber() {
        const WORDS: [&str; 10] = [
            "tracing_subscriber",
            "tracing::subscriber::",
            "subscriber::",
            "tracing::dispatcher",
            "dispatcher::",
            "tracing_core::dispatcher",
            "Dispatch::",
            "set_global_default",
            "with_default",
            "with_subscriber",
        ];
        // Each production line of a `main` that holds a scan word, and the one line of the log-writer test in secret.rs
        // that builds its subscriber, by exact line and count.
        const USE_INIT: &str = "use tracing_subscriber::util::SubscriberInitExt;";
        const USE_FILTER: &str = "use tracing_subscriber::EnvFilter;";
        const FMT: &str = "tracing_subscriber::fmt()";
        const BACKEND_MAIN: [(&str, usize); 4] = [
            (USE_INIT, 1),
            (USE_FILTER, 1),
            ("/// Writer for `tracing_subscriber::fmt`: mirrors every log line to BOTH", 1),
            (FMT, 1),
        ];
        const FRONTEND_MAIN: [(&str, usize); 3] = [(USE_INIT, 1), (USE_FILTER, 1), (FMT, 1)];
        const SECRET: [(&str, usize); 1] = [("let subscriber = tracing_subscriber::fmt()", 1)];
        let mut found = Vec::new();
        for (rel, text) in crate::test_scan::rust_sources() {
            let allowed: &[(&str, usize)] = match rel.as_str() {
                "rust/log/src/test_log.rs" => continue,
                "rust/backend/src/main.rs" => &BACKEND_MAIN,
                "rust/frontend/src/main.rs" => &FRONTEND_MAIN,
                "rust/log/src/secret.rs" => &SECRET,
                _ => &[],
            };
            let mut used = std::collections::HashMap::new();
            for (n, line) in text.lines().enumerate() {
                if !WORDS.iter().any(|w| line.contains(w)) {
                    continue;
                }
                let seen = used.entry(line.trim().to_string()).or_insert(0usize);
                *seen += 1;
                let room = allowed.iter().find(|(l, _)| *l == line.trim()).map_or(0, |(_, c)| *c);
                if *seen > room {
                    found.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
        assert!(found.is_empty(), "a subscriber is made outside test_log::capture:\n{}", found.join("\n"));
    }
}
