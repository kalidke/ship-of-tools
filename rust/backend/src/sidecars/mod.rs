//! The helper processes sotd supervises: the julia choice, kernel, REPL, Pluto, MathJax and the host monitor.

pub(super) mod julia;
pub(super) mod kernel;
pub(super) mod mathjax;
pub(super) mod monitor;
pub(super) mod ops;
pub(super) mod pluto;
pub(super) mod repl;

use serde::Serialize;
use serde_json::Value;

/// The request line a kernel or REPL child reads: `{v, id, kind, op, payload}`.
#[derive(Serialize)]
struct WireRequest<'a> {
    v: u32,
    id: u64,
    kind: &'a str,
    op: &'a str,
    payload: &'a Value,
}

/// What `f` logs at any level, as text: the one capture the sidecars' log-content tests share.
#[cfg(test)]
pub(super) fn logged_by(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}
