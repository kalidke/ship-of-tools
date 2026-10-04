// what a client reads and writes of a workspace: trees, preview, files, concept notes, kernel requests, math, transfers

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeRootReq {
    pub mode: String,
    /// Optional workspace this request is scoped to (ADR 0014). Missing
    /// resolves to the daemon's default workspace; back-compat with
    /// pre-0014 clients. Accepted as either a workspace_id or a slug
    /// (the backend resolves either).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeRootRes {
    pub node: TreeNode,
    #[serde(default)]
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeChildrenReq {
    pub node_id: String,
    /// See `TreeRootReq::workspace_id`. The backend uses this to route
    /// the children request to the right workspace's FilesMode walker.
    /// Important: node ids are scoped per-workspace, so a mismatched
    /// `workspace_id` will not find the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeChildrenRes {
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToggleHiddenReq {
    /// See `TreeRootReq::workspace_id`. Routes the toggle to the right
    /// workspace's FilesMode. `None` = default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Optional mode discriminator (only "files" today). Reserved for when
    /// another mode grows a hidden-toggle; ignored by the current handler.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToggleHiddenRes {
    /// The NEW state after the flip: true = hidden entries now shown.
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewGetReq {
    pub node_id: String,
    /// See `TreeRootReq::workspace_id`. The backend routes this to the
    /// owning workspace's FilesMode + Kernel so previews come from the
    /// right project's filesystem and (for `.jl` files) tokenizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// 1-based page for paginated previews (ADR 0021, e.g. PDFs). Absent =
    /// page 1 = pre-pagination behavior. Forwarded to the kernel as
    /// `file.preview {params: {page}}`; plugins clamp to the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Preview-pane size in pixels — a render-fit hint so rasterizing
    /// plugins (PDF) produce the page at display resolution and the GPU
    /// samples ~1:1 instead of aliasing through a resample. Optional and
    /// advisory; plugins without a use for it ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit_h: Option<u32>,
}

/// Mirrors `PreviewPayload` on the wire — the bytes follow the envelope and
/// arrive separately via the codec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewGetRes {
    pub mime: String,
    pub blob: BlobDescriptor,
    /// Plugin-reported metadata, forwarded verbatim from
    /// `PreviewPayload.extras` (ADR 0021). Opaque to the backend; the
    /// frontend reads the keys it knows (`page`, `page_count`) and ignores
    /// the rest — Rust never learns about new entity kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}

/// Persist a user-entered scale for a raster preview (ADR 0034 §5, live entry).
/// `physical_scale` is `{axes:[{name,nm_per_px}], unit}` per **ORIGINAL-image
/// pixel** (the raw pixel size the user typed; the FE has already converted
/// µm→nm). The backend writes it VERBATIM as `<image>.scale.json` and re-emits
/// `PreviewGetRes` with the F1 downsample rescale applied — never derive the
/// stored value from an emitted/rescaled one, or a read→write-back would
/// compound the ratio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewSetScaleReq {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub physical_scale: serde_json::Value,
}

/// Crop an image node's ROI (ADR 0022). `x,y,w,h` are in **source-image
/// pixel** coordinates; the backend clamps them to the decoded image bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageCropReq {
    pub node_id: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Result of `image.crop`: the backend filesystem path of the written PNG
/// (the in-pane LLM `Read`s this directly) plus the actual clamped crop
/// rectangle and the source image's native dimensions, so the caller can
/// report exactly what was captured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageCropRes {
    /// Absolute path to the written PNG on the backend host.
    pub path: String,
    /// The clamped crop rectangle actually used (source-image px).
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Source image native dimensions (px).
    pub src_w: u32,
    pub src_h: u32,
}

/// Generic kernel proxy. The backend forwards `(kernel_op, kernel_payload)`
/// to the Julia kernel and returns its response payload verbatim. Keeps
/// the main protocol stable while the kernel-side verb set grows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelRequestReq {
    pub kernel_op: String,
    #[serde(default)]
    pub kernel_payload: serde_json::Value,
    /// ADR 0014 workspace routing. Resolved to the per-workspace
    /// kernel handle so `file.parse` etc. see the
    /// right project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Read an annotation file from `<project_root>/.concept/<target>.md`. The
/// backend returns raw markdown; frontmatter parsing (notably the
/// `synced_against` AST-hash that gates drift indicators) is a frontend
/// concern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptReadReq {
    pub target: String,
    /// ADR 0014 workspace routing — annotations are scoped to their
    /// workspace's `.concept/` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptReadRes {
    pub target: String,
    pub exists: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptWriteReq {
    pub target: String,
    pub content: String,
    /// Optimistic-concurrency check. If `Some`, the backend reads the
    /// existing annotation file's frontmatter `synced_against` field and
    /// compares: when the on-disk hash differs from this value, the write
    /// is refused with `code: "stale_write"`. Callers that *want* to clobber
    /// regardless leave this `None` (the default; phase-1 backwards-compat).
    /// Skipped from the wire when `None` so older clients can keep sending
    /// the previous shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_ast_hash: Option<String>,
    /// ADR 0014 workspace routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptWriteRes {
    pub target: String,
    pub path: String,
    pub written: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptListRes {
    pub targets: Vec<String>,
}

/// Read a source file's full text for the in-frontend editor (distinct from
/// `preview.get`, which returns kernel-rendered preview bytes). `node_id` is
/// the same `files:<relpath>` id previews use, resolved against the workspace's
/// project root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadReq {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadRes {
    pub node_id: String,
    pub exists: bool,
    pub content: String,
    /// Opaque content version; pass it back as `FileWriteReq::expected_version`
    /// to make the save conflict-aware.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteReq {
    pub node_id: String,
    pub content: String,
    /// Optimistic-concurrency guard: the `version` from the matching
    /// `file.read`. If `Some` and the on-disk content changed since, the write
    /// is refused with `code: "conflict"` (the response carries the current
    /// on-disk content + version). `None` forces the write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteRes {
    pub node_id: String,
    pub path: String,
    /// New content version after the write.
    pub version: String,
    pub written: u64,
}

/// Trash a file from Files-mode nav (FE Ctrl+D). Directories are refused in
/// v1 (`code: "is_directory"`); a missing file is `code: "not_found"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDeleteReq {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDeleteRes {
    pub node_id: String,
    pub path: String,
    /// Always `true` in v1 — there is no hard-unlink path. Reserved so a
    /// future force-delete variant can answer `false`.
    pub trashed: bool,
    /// Recovery location when the in-workspace fallback was used
    /// (`<workspace_root>/.sot-trash/<ts>-<name>`); `None` when the file
    /// went to the system trash (`gio trash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_path: Option<String>,
}

/// Create a directory from Files-mode nav (FE Ctrl+N, a name ending in `/`).
/// Non-recursive: the parent must already exist, mirroring `file.write`'s
/// new-file contract. An existing file or directory at `node_id` is refused
/// with `code: "already_exists"` rather than silently succeeding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirCreateReq {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirCreateRes {
    pub node_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathRenderReq {
    pub latex: String,
    #[serde(default)]
    pub display: bool,
}

/// MathJax-rendered SVG. The bytes ride the wire as a blob; `ex` is the
/// MathJax ex-unit conversion factor so callers can size the result relative
/// to surrounding text. `display` echoes the request flag for clarity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathRenderRes {
    pub blob: BlobDescriptor,
    pub ex: f32,
    pub display: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryListReq {
    /// Absolute path whose immediate children are listed. Tilde is *not*
    /// expanded — frontends should resolve `~` to `$HOME` themselves.
    pub path: String,
    /// If true, entries whose name starts with `.` are included. Default
    /// false (skip hidden) — the Sessions-mode picker doesn't need them.
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    /// True if at least one subdirectory exists under this entry (cheap
    /// stat) — drives tree disclosure markers in the frontend.
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryListRes {
    /// Absolute path that was listed (echoes the request so the frontend
    /// can route the response to the right node).
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
}

/// `file.download` request — absolute backend-host path to stream down.
/// Read-only and unrestricted to the project root (matches `preview.get`'s
/// reach), since the user downloads files they navigated to anywhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadReq {
    pub path: String,
}

/// One streamed `file.download` chunk's metadata; the chunk bytes ride as the
/// frame's trailing blob (NOT in this JSON). Frames arrive in `offset` order
/// sharing the request id; `eof = true` marks the final chunk (which also
/// carries its bytes). `total` is the full file size for progress + prealloc.
/// `blob` is the codec's trailing-blob descriptor (`len` = this chunk's byte
/// count) — REQUIRED, or `codec::read_frame` won't consume the appended bytes
/// and the next frame desyncs onto raw file data. A download error instead
/// replies with `{error, code}` and no chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChunk {
    pub offset: u64,
    pub total: u64,
    pub eof: bool,
    pub blob: BlobDescriptor,
}

/// `file.upload` request — one chunk. The chunk bytes are base64 in `data_b64`
/// (in the JSON, not a trailing blob — keeps the incoming-frame path simple,
/// same as `pty.write`). `dir` is the absolute backend directory to drop into
/// (the cursored nav folder — anywhere the user navigated, not restricted to
/// project root); `name` is the picked file's basename, which the backend
/// sanitizes (rejects path separators / `..`, so it can't escape `dir`) and —
/// on `offset == 0` — de-duplicates with a ` (1)` suffix, returning the
/// resolved name in the ack. The frontend then sends chunks 1..N with `name`
/// set to that resolved name. The backend truncates on `offset == 0`, writes
/// at `offset`, finalizes on `eof`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadReq {
    pub dir: String,
    pub name: String,
    pub offset: u64,
    pub total: u64,
    pub eof: bool,
    pub data_b64: String,
}

/// `file.upload` per-chunk ack — lets the frontend flow-control (send the next
/// chunk on ack). `done = true` acks the final (`eof`) chunk; `final_name` is
/// then the basename actually written (post-sanitize, post ` (1)` de-dup).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadAck {
    pub offset: u64,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_name: Option<String>,
}
