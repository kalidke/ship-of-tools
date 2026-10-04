//! kernel.request ops: modules.list, project.scan, markdown.tokenize, file.parse, function.methods: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

pub(crate) async fn send_modules_list<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(?workspace_id, id, "→ kernel.request modules.list");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::KERNEL_REQUEST,
            serde_json::to_value(KernelRequestReq {
                kernel_op: "modules.list".to_string(),
                kernel_payload: serde_json::json!({}),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    // Capture the ws into the pending entry so the reply is
    // keyable (tree-provenance redesign).
    pending.insert(id, PendingKind::ModulesList { workspace_id });
    Ok(())
}

pub(crate) async fn send_project_scan<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    workspace_id: Option<String>,
    generation: u64,
) -> Result<()> {
    tracing::debug!(?workspace_id, generation, id, "→ kernel.request project.scan");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::KERNEL_REQUEST,
            serde_json::to_value(KernelRequestReq {
                kernel_op: "project.scan".to_string(),
                kernel_payload: serde_json::json!({}),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::ProjectScan { workspace_id, generation });
    Ok(())
}

pub(crate) async fn send_markdown_tokenize<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    lang: String,
    source_hash: u64,
    source: String,
) -> Result<()> {
    tracing::debug!(%lang, source_hash, id, "→ kernel.request markdown.tokenize");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::KERNEL_REQUEST,
            serde_json::to_value(KernelRequestReq {
                kernel_op: "markdown.tokenize".to_string(),
                kernel_payload: serde_json::json!({
                    "lang": lang,
                    "source": source,
                }),
                workspace_id: None,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::MarkdownTokenize { lang, source_hash });
    Ok(())
}

pub(crate) async fn send_file_parse<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%path, ?workspace_id, id, "→ kernel.request file.parse");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::KERNEL_REQUEST,
            serde_json::to_value(KernelRequestReq {
                kernel_op: "file.parse".to_string(),
                kernel_payload: serde_json::json!({ "path": path }),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FileParse { path, workspace_id });
    Ok(())
}

pub(crate) async fn send_function_methods<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    module: String,
    name: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%module, %name, ?workspace_id, id, "→ kernel.request function.methods");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::KERNEL_REQUEST,
            serde_json::to_value(KernelRequestReq {
                kernel_op: "function.methods".to_string(),
                kernel_payload: serde_json::json!({
                    "module": module,
                    "name": name,
                }),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::FunctionMethods { module, name, workspace_id });
    Ok(())
}
