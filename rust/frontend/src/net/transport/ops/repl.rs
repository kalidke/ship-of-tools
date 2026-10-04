//! repl.eval, repl.interrupt, repl.run_file: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

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
) -> Result<()> {
    tracing::info!(?workspace_id, id, "→ repl.interrupt");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::REPL_INTERRUPT,
            serde_json::json!({ "workspace_id": workspace_id }),
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
