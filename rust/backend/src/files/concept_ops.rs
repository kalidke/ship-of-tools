//! The concept ops: `concept.read`, `concept.write`, `concept.list` over a row's `.concept/` store.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::ConceptListRes;
use sot_protocol::ConceptReadReq;
use sot_protocol::ConceptReadRes;
use sot_protocol::ConceptWriteReq;
use sot_protocol::ConceptWriteRes;
use sot_protocol::Frame;
use crate::session::Session;
use crate::workspaces::Workspaces;
use crate::handlers::HandlerOutput;

pub async fn handle_concept_read(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ConceptReadReq =
        serde_json::from_value(payload_json).context("concept.read payload")?;
    tracing::info!(
        target = %req.target,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "concept.read"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_READ,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();
    // `ConceptStore::read` is a synchronous `std::fs::read_to_string` — real
    // blocking I/O, so it runs via `spawn_blocking` rather than inline on
    // this async task.
    let target_for_blk = req.target.clone();
    let read_result = tokio::task::spawn_blocking(move || concept.read(&target_for_blk))
        .await
        .context("concept.read task")?;
    let (exists, content) = match read_result {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_read_failed",
                "target": req.target,
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_READ, payload), None)]);
        }
    };

    let res = ConceptReadRes {
        target: req.target.clone(),
        exists,
        content,
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_READ, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_concept_write(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ConceptWriteReq =
        serde_json::from_value(payload_json).context("concept.write payload")?;
    tracing::info!(
        target = %req.target,
        len = req.content.len(),
        expected_set = req.expected_ast_hash.is_some(),
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "concept.write"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_WRITE,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();

    // Optimistic-concurrency check: if the client passed
    // `expected_ast_hash`, compare it against the on-disk annotation's
    // frontmatter `synced_against`. The check exists so a stale frontend
    // doesn't silently clobber an annotation that was already updated to
    // track a newer entity hash. If `expected_ast_hash` is None or the
    // on-disk file has no frontmatter, the write proceeds (phase-1 back-
    // compat).
    if let Some(expected) = req.expected_ast_hash.as_deref() {
        match concept.read_synced_against(&req.target) {
            Ok(Some(actual)) if actual != expected => {
                let payload = json!({
                    "error": "stale write: on-disk synced_against differs from expected",
                    "code": "stale_write",
                    "target": req.target,
                    "expected": expected,
                    "actual": actual,
                });
                return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
            }
            Ok(_) => {} // no frontmatter or no field on disk — nothing to be stale against
            Err(e) => {
                // I/O failure reading the on-disk file — surface as a distinct
                // failure code so the client can distinguish from a true
                // stale_write.
                let payload = json!({
                    "error": format!("{e:#}"),
                    "code": "concept_read_for_check_failed",
                    "target": req.target,
                });
                return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
            }
        }
    }

    let (path, written) = match concept.write(&req.target, &req.content) {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_write_failed",
                "target": req.target,
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_WRITE, payload), None)]);
        }
    };

    let res = ConceptWriteRes {
        target: req.target.clone(),
        path: path.to_string_lossy().to_string(),
        written,
    };
    // Annotation writes mutate the project, so bump the session revision —
    // a reconnecting client wants to know a concept file changed.
    let rev = session
        .bump("concept.written", json!({ "target": req.target }))
        .await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_WRITE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_concept_list(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let workspace_id = payload_json
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    tracing::info!(
        workspace_id = workspace_id.as_deref().unwrap_or("<default>"),
        "concept.list"
    );
    let Some(ws) = workspaces.resolve(workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::CONCEPT_LIST,
                json!({
                    "error": format!("unknown workspace: {:?}", workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let concept = ws.concept();
    let targets = match concept.list() {
        Ok(v) => v,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "concept_list_failed",
            });
            return Ok(vec![(Frame::res(req_id, op::CONCEPT_LIST, payload), None)]);
        }
    };
    let res = ConceptListRes { targets };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::CONCEPT_LIST, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}
