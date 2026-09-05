// sot-protocol
//
// Shared types for the JSON line protocol between frontend, backend, and kernel.
// Wire format: NDJSON envelopes — one JSON object per line, UTF-8, `\n`-terminated.
// Blob payloads are length-prefixed binary frames following an envelope whose
// payload contains `"blob": {"len": N, "mime": "…"}`. See docs/adr/0001.
//
// The IR types (`TreeNode`, `PreviewPayload`) mirror the Julia types in
// `core/src/ConceptExplorerCore.jl` so the same JSON shape works on both
// sides of the Rust↔Julia seam.

pub mod codec;
pub mod ir;
pub mod ops;
pub mod session_socket;

pub use codec::{read_frame, write_frame};
pub use ir::{BlobDescriptor, PreviewPayload, TreeNode};
pub use ops::{
    op, AgentSendReq, AgentSendRes, ConceptListRes, ConceptReadReq, ConceptReadRes,
    ConceptWriteReq, ConceptWriteRes, DirectoryEntry, DirectoryListReq, DirectoryListRes,
    DocsOpenReq, DocsOpenRes, FeCommandEvt, FeCommandSendReq, FeCommandSendRes, FileChunk,
    FileDeleteReq, FileDeleteRes, FileDownloadReq, FileReadReq, FileReadRes, FileUploadAck,
    FileUploadReq, FileWriteReq, FileWriteRes, GpuSample, HelloReq, HelloRes, HostLatest,
    HostSeries, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    MonitorHistoryReq, MonitorHistoryRes, MonitorSample, MonitorSubscribeReq, MonitorSubscribeRes,
    MonitorTickEvt, MonitorUnsubscribeReq, PlutoOpenReq, PlutoOpenRes, PreviewGetReq,
    PreviewGetRes, PreviewSetScaleReq, ProcSample, ProxyConnectReq, ProxyConnectRes, PtyEvt,
    PtyOpenReq, PtyOpenRes, PtyResizeReq, PtyScrollReq, PtyWriteReq, QuartoOpenReq, QuartoOpenRes,
    ReplErrorOut, ReplEvalReq, ReplEvalRes, ReplExecuteInput, ReplExecuteReq, ReplExecuteRes,
    ReplFrame, ReplFrameEvt, ReplRunFileReq, ReplRunFileRes, ReplValueOut, StackFrame,
    TmuxCapturePaneReq, TmuxCapturePaneRes, TmuxCreateSessionReq, TmuxKillSessionReq,
    TmuxListPanesReq, TmuxListPanesRes, TmuxListSessionsRes, TmuxPane, TmuxSession,
    ToggleHiddenReq, ToggleHiddenRes, TreeChildrenReq, TreeChildrenRes, TreeRootReq, TreeRootRes,
    UpdateApplyReq, UpdateApplyRes, UpdateCheckReq, UpdateCheckRes, VideoOpenReq, VideoOpenRes,
    WorkspaceActivateReq, WorkspaceActivateRes, WorkspaceCreateReq, WorkspaceCreateRes,
    WorkspaceDestroyReq, WorkspaceDestroyRes, WorkspaceListEntry, WorkspaceListReq,
    WorkspaceListRes,
};
// `is_private_dir` stays module-private to `session_socket` (not
// re-exported here): nothing outside that module calls it directly
// (`runtime_sot_dir` is its only caller), so a crate-root re-export would
// be dead weight -- codex follow-up, ADR 0042 L2b.
pub use session_socket::{current_uid, runtime_sot_dir, session_socket_path, slug};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Product version embedded at build time (ADR 0030 §1): `X.Y.Z` when the
/// build sits exactly on its release tag `vX.Y.Z`, otherwise
/// `X.Y.Z-dev+<sha>`. Plain `X.Y.Z` when built without git (release
/// tarballs). The `-dev` marker is what gates the auto-updater — a dev build
/// must never self-update.
pub fn app_version() -> String {
    let pkg = env!("CARGO_PKG_VERSION");
    let sha = env!("SOT_BUILD_SHA");
    if env!("SOT_BUILD_ON_TAG") == "1" || sha.is_empty() {
        pkg.to_string()
    } else {
        format!("{pkg}-dev+{sha}")
    }
}

/// One-line `--version` output: `<bin> <version> (<sha> <date>)`, or
/// `<bin> <version>` when built without git.
pub fn version_line(bin: &str) -> String {
    let sha = env!("SOT_BUILD_SHA");
    let date = env!("SOT_BUILD_DATE");
    if sha.is_empty() {
        format!("{bin} {}", app_version())
    } else {
        format!("{bin} {} ({sha} {date})", app_version())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Req,
    Res,
    Evt,
}

/// Wire envelope. Payload is left as `serde_json::Value` so the codec can
/// route on `op` and inspect `payload.blob` without baking every op into a
/// single enum — keeps the protocol crate small and lets new ops land
/// without churning shared structs.
///
/// `rev`, when set, carries the session revision the frame represents. Per
/// ADR 0010 the frontend tracks the highest seen revision and feeds it into
/// the next `hello` so the backend can replay events the client missed.
/// Events from the replay path always carry `rev`; bare control responses
/// may carry it too when the op mutated session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub v: u32,
    pub id: u64,
    pub kind: Kind,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    pub payload: serde_json::Value,
}

impl Frame {
    pub fn req(id: u64, op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind: Kind::Req,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    pub fn res(id: u64, op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind: Kind::Res,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    pub fn evt(op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: 0,
            kind: Kind::Evt,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    /// Stamp this frame with the session revision it represents.
    pub fn with_rev(mut self, r: u64) -> Self {
        self.rev = Some(r);
        self
    }
}
