//! kernel.request ops: project.scan, markdown.tokenize, file.parse, function.methods: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

/// One row from `file.parse`'s `definitions[]`. Mirrors the kernel's
/// per-entity shape (name + kind + line + optional parent + per-entity
/// ast_hash). The chrome uses `name`/`kind` for rendering the module's
/// col-2 children and `ast_hash` for per-entity drift detection.
#[derive(Debug, Clone)]
pub struct DefinitionInfo {
    pub name: String,
    pub kind: String,
    #[allow(dead_code)] // future: jump-to-line UX
    pub line: i64,
    #[allow(dead_code)] // future: nested-entity grouping
    pub parent: Option<String>,
    #[allow(dead_code)] // future: per-entity drift badge
    pub ast_hash: Option<String>,
}

/// One backend-derived semantic span for a fenced code block. Returned
/// in source order by `kernel.request markdown.tokenize` per the
/// Codex-recommended tree-sitter-base + LSP-overlay architecture. Byte
/// offsets are 0-indexed, end-exclusive (matches Rust slice semantics).
/// `kind` is a tree-sitter standard capture name so the chrome can
/// route through the same `preview::highlight::color_for_scope` palette
/// the tree-sitter base layer uses.
#[derive(Debug, Clone)]
pub struct MarkdownToken {
    pub start: usize,
    pub end: usize,
    pub kind: String,
}

/// One module node from `kernel.request project.scan`. Modules nest
/// arbitrarily via `submodules`. Types carry their own constructors;
/// non-constructor functions live in `functions`. Each entity records
/// its file + line so the chrome's source-preview path knows where to
/// fire `preview.get`.
#[derive(Debug, Clone, Default)]
pub struct ScanModule {
    pub name: String,
    pub file: String,
    pub line: i64,
    pub ast_hash: String,
    pub types: Vec<ScanType>,
    pub functions: Vec<ScanEntity>,
    pub submodules: Vec<ScanModule>,
}

/// One type from `project.scan` — struct, mutable struct, abstract,
/// or primitive. Carries its constructors (functions whose name
/// matches the type's, merged inner + outer). Fields are not yet in
/// the v1 wire shape — follow-up once the unified mode is in use.
#[derive(Debug, Clone, Default)]
pub struct ScanType {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: i64,
    /// Carried for future per-entity drift detection. Same shape as
    /// the `file.parse` ast_hash field on `DefinitionInfo`.
    #[allow(dead_code)]
    pub ast_hash: String,
    pub constructors: Vec<ScanEntity>,
}

/// Generic non-module / non-type entity (functions, macros). Same
/// shape used for top-level functions and for constructors nested
/// under types.
#[derive(Debug, Clone, Default)]
pub struct ScanEntity {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: i64,
    #[allow(dead_code)] // future: per-entity drift badge
    pub ast_hash: String,
}

/// One method returned by `kernel.request function.methods`. Mirrors the
/// kernel reply (`b5faf94`). `sig` is the standard `string(m)` repr; the
/// chrome trims the trailing ` @ <module> <file>:<line>` for display.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub sig: String,
    #[allow(dead_code)] // future: jump-to-line + per-method drift
    pub file: String,
    #[allow(dead_code)] // future: jump-to-line
    pub line: i64,
    #[allow(dead_code)] // future: per-method drift badge
    pub ast_hash: Option<String>,
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

pub(crate) fn on_project_scan(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    workspace_id: Option<String>,
    generation: u64,
) {
    // KERNEL_REQUEST returns the kernel's response payload
    // verbatim. project.scan shape is described in
    // ShipToolsKernel.handle_project_scan: `{project_root,
    // package_name, entry_file, modules: [...]}`.
    let payload = frame.payload;
    let project_root = payload
        .get("project_root")
        .and_then(|v| v.as_str())
        .map(String::from);
    let package_name = payload
        .get("package_name")
        .and_then(|v| v.as_str())
        .map(String::from);
    let entry_file = payload
        .get("entry_file")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
        tracing::warn!(error = %err, "project.scan returned error");
        emit(IncomingEvt::ProjectScan {
            workspace_id,
            project_root,
            package_name,
            entry_file,
            modules: Vec::new(),
            generation,
        });
    } else {
        let modules = payload
            .get("modules")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(parse_scan_module).collect())
            .unwrap_or_default();
        emit(IncomingEvt::ProjectScan {
            workspace_id,
            project_root,
            package_name,
            entry_file,
            modules,
            generation,
        });
    }
}

pub(crate) fn on_markdown_tokenize(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    lang: String,
    source_hash: u64,
) {
    // Wire shape: `{ lang, spans: [{ start, end, kind }] }`.
    // We echo `source_hash` from our pending state back to the
    // chrome so it can route into the per-fence cache without
    // the backend knowing about our hashing scheme.
    let payload = frame.payload;
    let spans = payload
        .get("spans")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let start = s.get("start")?.as_u64()? as usize;
                    let end = s.get("end")?.as_u64()? as usize;
                    let kind = s.get("kind")?.as_str()?.to_string();
                    Some(MarkdownToken { start, end, kind })
                })
                .collect()
        })
        .unwrap_or_default();
    emit(IncomingEvt::MarkdownTokens {
        lang,
        source_hash,
        spans,
    });
}

pub(crate) fn on_file_parse(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    path: String,
    workspace_id: Option<String>,
) {
    // `file.parse` returns either {ast_hash, path, definitions}
    // or {error, code, ast_hash?} on parse failure. The hash
    // is computed from raw bytes before the parser runs, so
    // it's present even on parse failure; the definitions
    // array is absent or empty in that case. Outright kernel
    // errors (file missing / outside root) leave both absent;
    // surface nothing then so the chrome stays neutral.
    let hash = frame
        .payload
        .get("ast_hash")
        .and_then(|v| v.as_str())
        .map(String::from);
    let Some(ast_hash) = hash else {
        // warn, not debug: this silently wedged the drift badge
        // at "checking…" for a whole capture run before anyone
        // saw the actual error payload (2026-07-02).
        tracing::warn!(
            %path,
            payload = %frame.payload,
            "file.parse returned no ast_hash — drift check failed, un-latching for retry"
        );
        emit(IncomingEvt::FileParseFailed { workspace_id, path });
        return;
    };
    let definitions: Vec<DefinitionInfo> = frame
        .payload
        .get("definitions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    let name = d.get("name").and_then(|v| v.as_str())?.to_string();
                    let kind = d
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let line = d.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                    let parent =
                        d.get("parent").and_then(|v| v.as_str()).map(String::from);
                    let ast_hash =
                        d.get("ast_hash").and_then(|v| v.as_str()).map(String::from);
                    Some(DefinitionInfo {
                        name,
                        kind,
                        line,
                        parent,
                        ast_hash,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    emit(IncomingEvt::FileParsed {
        workspace_id,
        path,
        ast_hash,
        definitions,
    });
}

pub(crate) fn on_function_methods(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    module: String,
    name: String,
    workspace_id: Option<String>,
) {
    // Reply shape: `{methods: [{module, name, file, line, sig, ast_hash}, ...]}`
    // or `{error, code}` on bad_request / module_not_found /
    // function_not_found. We surface an empty list in the
    // error case so the chrome still applies (no children),
    // rather than leaving the row in a "loading…" limbo.
    let methods: Vec<MethodInfo> = frame
        .payload
        .get("methods")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let sig = m.get("sig").and_then(|v| v.as_str())?.to_string();
                    let file = m
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let line = m.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                    let ast_hash =
                        m.get("ast_hash").and_then(|v| v.as_str()).map(String::from);
                    Some(MethodInfo {
                        sig,
                        file,
                        line,
                        ast_hash,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str()) {
        tracing::warn!(%module, %name, code, "function.methods returned error");
    }
    emit(IncomingEvt::FunctionMethodsReceived {
        workspace_id,
        module,
        name,
        methods,
    });
}

/// Convert one entry from `project.scan`'s `modules: [...]` array into
/// a [`ScanModule`]. Field shape matches ShipToolsKernel.handle_project_scan
/// in `julia/kernel/src/ShipToolsKernel.jl`. Tolerant of missing fields —
/// the kernel always emits the canonical keys, but if a future version
/// adds optionals or omits something on the error path the chrome
/// degrades to defaults instead of dropping the whole tree.
fn parse_scan_module(v: &Value) -> ScanModule {
    ScanModule {
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        file: v
            .get("file")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        line: v.get("line").and_then(|x| x.as_i64()).unwrap_or(0),
        ast_hash: v
            .get("ast_hash")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        types: v
            .get("types")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().map(parse_scan_type).collect())
            .unwrap_or_default(),
        functions: v
            .get("functions")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().map(parse_scan_entity).collect())
            .unwrap_or_default(),
        submodules: v
            .get("submodules")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().map(parse_scan_module).collect())
            .unwrap_or_default(),
    }
}

fn parse_scan_type(v: &Value) -> ScanType {
    ScanType {
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        kind: v
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        file: v
            .get("file")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        line: v.get("line").and_then(|x| x.as_i64()).unwrap_or(0),
        ast_hash: v
            .get("ast_hash")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        constructors: v
            .get("constructors")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().map(parse_scan_entity).collect())
            .unwrap_or_default(),
    }
}

fn parse_scan_entity(v: &Value) -> ScanEntity {
    ScanEntity {
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        kind: v
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        file: v
            .get("file")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        line: v.get("line").and_then(|x| x.as_i64()).unwrap_or(0),
        ast_hash: v
            .get("ast_hash")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}
