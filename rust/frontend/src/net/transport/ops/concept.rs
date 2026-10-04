//! concept.read, concept.write: the requests (send_<op>: write the frame, then record its PendingKind).

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
