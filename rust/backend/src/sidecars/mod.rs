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
