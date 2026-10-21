use super::*;
use sot_protocol::{ReplFrame, ReplFrameEvt};

fn shown(rx: &std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>) -> Vec<String> {
    rx.try_iter().map(|(_, e)| format!("{e:?}")).collect()
}

#[test]
fn a_math_render_error_reply_ends_its_wait() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host: HostKey = "h".into();
    let mut pending: HashMap<u64, PendingKind> = HashMap::new();
    pending.insert(13, PendingKind::MathRender { latex: "x^2".into(), display: true });
    let frame = Frame::res(
        13,
        op::MATH_RENDER,
        serde_json::json!({ "error": "mathjax render timed out after 10s", "code": "mathjax_render_failed" }),
    );
    handle_response_frame(frame, None, &mut pending, &evt_tx, &host);
    let events = shown(&evt_rx);
    println!("math-render-error: events={events:?}");
    assert!(
        matches!(events.as_slice(), [e] if e.starts_with("MathRenderFailed")),
        "an error reply did not end the render's wait"
    );
}

#[test]
fn a_lost_math_render_is_failed_when_the_connection_ends() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(13, PendingKind::MathRender { latex: "x^2".into(), display: true });
    }
    let events = shown(&evt_rx);
    println!("math-render-lost: events={events:?}");
    assert!(
        matches!(events.as_slice(), [e] if e.starts_with("MathRenderFailed")),
        "no MathRenderFailed after the guard dropped"
    );
}

#[test]
fn a_lost_markdown_tokenize_ends_when_the_connection_ends() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(11, PendingKind::MarkdownTokenize { lang: "julia".into(), source_hash: 102 });
    }
    let events = shown(&evt_rx);
    println!("markdown-tokenize-lost: events={events:?}");
    assert_eq!(
        events,
        ["MarkdownTokens { lang: \"julia\", source_hash: 102, spans: [] }"],
        "no end after the guard dropped"
    );
}

/// An eval the daemon accepted, and a run another client started, end with one error frame when their connection
/// ends before their `done` frame; one whose `done` came is not ended twice.
#[test]
fn a_running_eval_ends_when_its_connection_ends() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host: HostKey = "h".into();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: host.clone() };
        guard.insert(4, PendingKind::ReplEval { eval_id: 8 });
        guard.insert(5, PendingKind::ReplEval { eval_id: 7 });
        for (id, eval_id) in [(4, 8), (5, 7)] {
            let ack = serde_json::json!({ "eval_id": eval_id, "elapsed_ms": 0, "frames": [], "accepted": true });
            handle_response_frame(Frame::res(id, op::REPL_EVAL, ack), None, &mut guard, &evt_tx, &host);
        }
        let evt = |eval_id: u64, frame: ReplFrame| {
            Frame::evt(
                op::REPL_FRAME,
                serde_json::to_value(ReplFrameEvt { eval_id, workspace_id: None, frame }).unwrap(),
            )
        };
        let started = ReplFrame::Started { run_id: "r".into(), origin: "session".into(), display: "run.jl".into() };
        handle_response_frame(evt(30, started), None, &mut guard, &evt_tx, &host);
        handle_response_frame(evt(7, ReplFrame::Done { eval_id: 7, elapsed_ms: 5 }), None, &mut guard, &evt_tx, &host);
        let _ = shown(&evt_rx);
    }
    let mut ended: Vec<String> = shown(&evt_rx);
    ended.sort();
    println!("repl-eval-lost: events at the connection's end={ended:?}");
    assert!(
        ended.len() == 2 && ended.iter().all(|e| e.starts_with("ReplEvalDone") && e.contains("Error")),
        "the connection's end did not end evals 8 and 30 (and only those)"
    );
}

/// An eval and an `r`/`R` run whose request was written but whose ack had not come when the connection ended are
/// ended too, each with the one error frame. The request may or may not have reached the daemon; if it did and the
/// run is still going, that hello names it (`HelloRes::repl_running`) and the window reopens it, whether the REPL
/// has accepted it yet or is still starting; a run the daemon accepts after the hello is not named.
#[test]
fn an_eval_or_run_whose_ack_had_not_come_ends_when_its_connection_ends() {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    {
        let mut guard = PendingGuard { map: HashMap::new(), evt_tx: &evt_tx, host: "h".into() };
        guard.insert(4, PendingKind::ReplEval { eval_id: 9 });
        guard.insert(5, PendingKind::ReplRunFile { eval_id: 10, path: "x.jl".into(), fresh: false });
    }
    let mut ended = shown(&evt_rx);
    ended.sort();
    println!("repl-unacked: events at the connection's end={ended:?}");
    let lost = |eval_id: u64| {
        format!(
            "ReplEvalDone {{ eval_id: {eval_id}, elapsed_ms: 0, frames: [Error {{ message: \"the connection to the \
                 daemon ended before this eval's end\", stacktrace: [] }}] }}"
        )
    };
    assert_eq!(ended, [lost(10), lost(9)], "the evals whose ack had not come stayed running");
}
