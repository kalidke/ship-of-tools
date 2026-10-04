// sot-protocol
//
// Shared types for the JSON line protocol between frontend, backend, and kernel.
// Wire format: NDJSON envelopes — one JSON object per line, UTF-8, `\n`-terminated.
// Blob payloads are length-prefixed binary frames following an envelope whose
// payload contains `"blob": {"len": N, "mime": "…"}`. See docs/adr/0001.

pub mod codec;
pub mod ir;
// ADR 0045 decision 3: `DaemonLaneEndpoint` — the attach client's own
// `sot_log::client::Endpoint` for the lane bridge (`lane.connect`),
// alongside the platform pipe/socket endpoints `sot-log` itself owns.
// Lives here, not in `sot-log`, because it is a WIRE client of this
// crate's own `LaneConnectReq`/`LaneConnectRes` op — `sot-log` has no
// dependency on `sot-protocol` to build against.
pub use topology::lane_client;
pub mod ops;
pub use topology::endpoint as session_socket;
// C3 (isolation-plan.md §3, as amended by dev/output/c3-second-connection-
// amendment.md): the ssh child every non-local dial spawns -- one recipe
// type and one argv builder, shared by the control connection
// (`rust/frontend/src/transport.rs`), the lane attach (`lane_client.rs`
// above) and the page-proxy leg (`rust/frontend/src/proxy_listen.rs`).
pub use topology::ssh_bridge;
// The declared topology (`hosts.toml`, grammar v2): the ONE parser and the
// ONE search rule for the daemon, `sotd topology`, and the frontend.
pub mod topology;

pub use codec::{read_frame, write_frame};
pub use ir::{BlobDescriptor, PreviewPayload, TreeNode};
pub use ops::{
    op, AccountEntry, AccountsListReq, AccountsListRes, AgentFiledReq, AgentFiledRes,
    AgentJoinReq, AgentJoinRes, AgentReceiptEvt, AgentSendReq, AgentSendRes, ClientVersion, CommFileReq, CommFileRes, ConceptListRes, ConceptReadReq, ConceptReadRes,
    ConceptWriteReq, ConceptWriteRes, DaemonVersion, DeclaredSession, DisconnectedBox, DirCreateReq, DirCreateRes, DirectoryEntry,
    DirectoryListReq, DirectoryListRes, DocsOpenReq, DocsOpenRes, FeCommandEvt, FeCommandSendReq,
    FeCommandSendRes,
    FePresenceReq, FePresenceRes, FeSessionsReq, FeSessionsRes, FileChunk, FileDeleteReq, FileDeleteRes, FileDownloadReq,
    FileReadReq, FileReadRes,
    FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, GpuSample, HelloReq, HelloRes,
    HostLatest, HostSeries, ImageCropReq, ImageCropRes, KernelRequestReq, LaneConnectReq,
    LaneConnectRes, MathRenderReq, MathRenderRes, MonitorHistoryReq, MonitorHistoryRes,
    MonitorSample, MonitorSubscribeReq,
    MonitorSubscribeRes, MonitorTickEvt, MonitorUnsubscribeReq, PingReq, PingRes, PlutoOpenReq, PlutoOpenRes,
    PreviewGetReq, PreviewGetRes, PreviewSetScaleReq, ProcSample, ProxyConnectReq, ProxyConnectRes,
    PtyCursor, PtyEnter, PtyEvt, PtyInputReq, PtyInputRes, PtyOpenReq, PtyOpenRes, PtyResizeReq,
    PtyScreenReq, PtyScreenRes, PtyScrollReq, PtyWriteReq, QuartoOpenReq, QuartoOpenRes,
    ReplErrorOut, ReplEvalReq, ReplEvalRes, ReplExecuteInput, ReplExecuteReq, ReplExecuteRes,
    ReplFrame, ReplFrameEvt, ReplRunFileReq, ReplRunFileRes, ReplValueOut, StackFrame,
    ToggleHiddenReq, ToggleHiddenRes, TopologySetReq, TopologySetRes, TreeChildrenReq, TreeChildrenRes, TreeRootReq, TreeRootRes,
    UpdateApplyReq, UpdateApplyRes, UpdateCheckReq, UpdateCheckRes, VersionQueryReq,
    VersionQueryRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceActivateRes,
    WorkspaceCreateReq, WorkspaceCreateRes, WorkspaceDestroyReq, WorkspaceDestroyRes,
    WorkspaceListEntry, WorkspaceListReq, WorkspaceListRes, WorkspaceReauthReq,
    WorkspaceReauthRes,
};
// `is_private_dir` stays module-private to `session_socket` (not
// re-exported here): nothing outside that module calls it directly
// (`runtime_sot_dir` is its only caller), so a crate-root re-export would
// be dead weight -- codex follow-up, ADR 0042 L2b.
pub use session_socket::{current_uid, local_daemon_label, runtime_sot_dir, session_socket_path, slug};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;

mod version;
pub use version::{app_version, is_release_build, version_line};

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
