//! repl.eval, repl.interrupt, repl.run_file: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

/// Success payload for `repl.run_file`. Carries the canonical fields the
/// chrome surfaces in the status line plus the frame list for any future
/// out-of-band routing (e.g. mirroring the last image frame to the
/// preview pane — TODO row 161).
#[derive(Debug, Clone)]
pub struct ReplRunFileInfo {
    pub eval_id: u64,
    pub path: String,
    pub fresh: bool,
    pub elapsed_ms: u64,
    pub project_dir: Option<String>,
    #[allow(dead_code)] // useful when frontend wants to distinguish
    // discovered vs fallback for status copy
    pub project_source: Option<String>,
    pub frames: Vec<ReplFrame>,
}

pub(crate) async fn send_repl_eval<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    eval_id: u64,
    code: String,
    mode: Option<String>,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(eval_id, code_len = code.len(), ?mode, ?workspace_id, id, "→ repl.eval");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::REPL_EVAL,
            serde_json::to_value(ReplEvalReq {
                eval_id,
                code,
                mode,
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::ReplEval { eval_id });
    Ok(())
}

pub(crate) async fn send_repl_interrupt<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
    workspace_id: Option<String>,
    eval_ids: Vec<u64>,
) -> Result<()> {
    tracing::info!(?workspace_id, ?eval_ids, id, "→ repl.interrupt");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::REPL_INTERRUPT,
            serde_json::json!({ "workspace_id": workspace_id, "eval_ids": eval_ids }),
        ),
        None,
    )
    .await?;
    // Fire-and-forget: the interrupt's effect arrives as
    // streamed error+done frames, so there's no response to
    // track in `pending`.
    Ok(())
}

pub(crate) async fn send_repl_run_file<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    eval_id: u64,
    path: String,
    fresh: bool,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::info!(%path, fresh, eval_id, ?workspace_id, id, "→ repl.run_file");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::REPL_RUN_FILE,
            serde_json::to_value(ReplRunFileReq {
                eval_id,
                path: path.clone(),
                fresh,
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::ReplRunFile { eval_id, path, fresh });
    Ok(())
}

pub(crate) fn on_repl_eval(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    eval_id: u64,
) {
    // An error reply (`{error, code}`, whatever the code: the REPL's own,
    // `unknown_workspace`, `handler_error`) ends the eval with one error frame
    // naming the daemon's words and code, as `on_repl_run_file` ends a run.
    // Nothing else ends an eval the daemon refused: it streams no frames.
    if let Some(message) = error_text(&frame.payload) {
        let frames = vec![ReplFrame::Error { message, stacktrace: Vec::new() }];
        emit(IncomingEvt::ReplEvalDone { eval_id, elapsed_ms: 0, frames });
        return;
    }
    // Synchronous-collect per ADR 0009: the response carries
    // the full frame list. Streamed delivery is a planned
    // enhancement and shifts the routing
    // off this path; this arm only needs to handle the
    // collected-at-once payload.
    match serde_json::from_value::<ReplEvalRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::ReplEvalDone {
                eval_id: res.eval_id,
                elapsed_ms: res.elapsed_ms,
                frames: res.frames,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, eval_id, "repl.eval res parse failed");
        }
    }
}

pub(crate) fn on_repl_run_file(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    eval_id: u64,
    path: String,
    fresh: bool,
) {
    // Success = `frames` present; error envelopes carry
    // `{error, code}` per the handler contract. Same shape
    // pattern as WorkspaceCreate / PlutoOpen above.
    let payload = frame.payload;
    let result = if payload.get("frames").is_some() {
        match serde_json::from_value::<ReplRunFileRes>(payload) {
            Ok(r) => Ok(ReplRunFileInfo {
                eval_id: r.eval_id,
                path: r.path,
                fresh: r.fresh,
                elapsed_ms: r.elapsed_ms,
                project_dir: r.project_dir,
                project_source: r.project_source,
                frames: r.frames,
            }),
            Err(e) => Err(format!("repl.run_file res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    let _ = path;
    let _ = fresh;
    emit(IncomingEvt::ReplRunFileDone { eval_id, result });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `on_repl_eval` emits for one reply payload.
    fn emitted(payload: serde_json::Value) -> Vec<IncomingEvt> {
        let got = std::cell::RefCell::new(Vec::new());
        on_repl_eval(Frame::res(9, op::REPL_EVAL, payload), &|e| got.borrow_mut().push(e), 8);
        got.into_inner()
    }

    /// Every error reply to an eval ends it with one error frame naming the daemon's words and code; the acceptance
    /// ack still only acknowledges.
    #[test]
    fn an_error_reply_ends_the_eval_with_one_error_frame() {
        for (code, error) in [
            ("repl_busy", "the REPL's queue is full"),
            ("repl_eval_failed", "repl supervisor channel closed"),
            ("unknown_workspace", "unknown workspace: Some(\"gone\")"),
            ("handler_error", "repl.eval payload"),
        ] {
            let events = emitted(serde_json::json!({ "error": error, "code": code }));
            let shown: Vec<String> = events
                .iter()
                .map(|e| match e {
                    IncomingEvt::ReplEvalDone { eval_id, elapsed_ms, frames } => match frames.as_slice() {
                        [ReplFrame::Error { message, stacktrace }] if stacktrace.is_empty() => {
                            format!("done eval_id={eval_id} elapsed_ms={elapsed_ms} error={message:?}")
                        }
                        other => format!("done eval_id={eval_id} frames={other:?}"),
                    },
                    _ => "another event".to_string(),
                })
                .collect();
            println!("repl-eval-error: code={code} events={shown:?}");
            assert_eq!(shown, [format!("done eval_id=8 elapsed_ms=0 error={:?}", format!("{error} [{code}]"))]);
        }
        let ack = emitted(serde_json::json!({ "eval_id": 8, "elapsed_ms": 0, "frames": [], "accepted": true }));
        assert!(
            matches!(ack.as_slice(), [IncomingEvt::ReplEvalDone { eval_id: 8, elapsed_ms: 0, frames }] if frames.is_empty()),
            "the acceptance ack is an empty ReplEvalDone"
        );
    }
}
