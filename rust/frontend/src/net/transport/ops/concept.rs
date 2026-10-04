//! concept.read, concept.write: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

pub(crate) async fn send_concept_read<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    target: String,
    workspace_id: Option<String>,
    generation: u64,
) -> Result<()> {
    tracing::debug!(%target, ?workspace_id, generation, id, "→ concept.read");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::CONCEPT_READ,
            serde_json::to_value(ConceptReadReq {
                target: target.clone(),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(
        id,
        PendingKind::ConceptRead {
            target,
            workspace_id,
            generation,
        },
    );
    Ok(())
}

pub(crate) async fn send_concept_write<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    target: String,
    content: String,
    expected_ast_hash: Option<String>,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(
        %target,
        bytes = content.len(),
        ast_hash = ?expected_ast_hash,
        ?workspace_id,
        id,
        "→ concept.write"
    );
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::CONCEPT_WRITE,
            serde_json::to_value(ConceptWriteReq {
                target: target.clone(),
                content,
                expected_ast_hash,
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::ConceptWrite { target });
    Ok(())
}

pub(crate) fn on_concept_read(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    target: String,
    workspace_id: Option<String>,
    generation: u64,
) {
    match serde_json::from_value::<ConceptReadRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::ConceptRead {
                target: res.target,
                workspace_id,
                exists: res.exists,
                content: res.content,
                generation,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, %target, "concept.read res parse failed");
        }
    }
}

pub(crate) fn on_concept_write(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    target: String,
) {
    // Three shapes: the happy-path `ConceptWriteRes`, a
    // `stale_write` envelope (`{error, code: "stale_write",
    // ...}`), or any other `{error, code, ...}` failure.
    // Surface the right variant so the chrome can react
    // without re-parsing the wire shape itself.
    let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
    {
        if code == "stale_write" {
            ConceptWriteResult::Stale
        } else {
            let message = frame
                .payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("(no message)")
                .to_string();
            ConceptWriteResult::Error {
                code: code.to_string(),
                message,
            }
        }
    } else {
        match serde_json::from_value::<ConceptWriteRes>(frame.payload) {
            Ok(res) => ConceptWriteResult::Ok {
                path: res.path,
                written: res.written,
            },
            Err(e) => {
                tracing::warn!(error = %e, %target,
                    "concept.write res parse failed");
                ConceptWriteResult::Error {
                    code: "parse_failed".to_string(),
                    message: e.to_string(),
                }
            }
        }
    };
    emit(IncomingEvt::ConceptWriteDone { target, result });
}
