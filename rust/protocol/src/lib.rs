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
    op, AgentSendReq, AgentSendRes, ClientVersion, ConceptListRes, ConceptReadReq, ConceptReadRes,
    ConceptWriteReq, ConceptWriteRes, DaemonVersion, DirectoryEntry, DirectoryListReq,
    DirectoryListRes, DocsOpenReq, DocsOpenRes, FeCommandEvt, FeCommandSendReq, FeCommandSendRes,
    FileChunk, FileDeleteReq, FileDeleteRes, FileDownloadReq, FileReadReq, FileReadRes,
    FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, GpuSample, HelloReq, HelloRes,
    HostLatest, HostSeries, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq,
    MathRenderRes, MonitorHistoryReq, MonitorHistoryRes, MonitorSample, MonitorSubscribeReq,
    MonitorSubscribeRes, MonitorTickEvt, MonitorUnsubscribeReq, PlutoOpenReq, PlutoOpenRes,
    PreviewGetReq, PreviewGetRes, PreviewSetScaleReq, ProcSample, ProxyConnectReq, ProxyConnectRes,
    PtyCursor, PtyEvt, PtyInputReq, PtyInputRes, PtyOpenReq, PtyOpenRes, PtyResizeReq,
    PtyScreenReq, PtyScreenRes, PtyScrollReq, PtyWriteReq, QuartoOpenReq, QuartoOpenRes,
    ReplErrorOut, ReplEvalReq, ReplEvalRes, ReplExecuteInput, ReplExecuteReq, ReplExecuteRes,
    ReplFrame, ReplFrameEvt, ReplRunFileReq, ReplRunFileRes, ReplValueOut, StackFrame,
    TmuxCapturePaneReq, TmuxCapturePaneRes, TmuxCreateSessionReq, TmuxKillSessionReq,
    TmuxListPanesReq, TmuxListPanesRes, TmuxListSessionsRes, TmuxPane, TmuxSession,
    ToggleHiddenReq, ToggleHiddenRes, TreeChildrenReq, TreeChildrenRes, TreeRootReq, TreeRootRes,
    UpdateApplyReq, UpdateApplyRes, UpdateCheckReq, UpdateCheckRes, VersionQueryReq,
    VersionQueryRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceActivateRes,
    WorkspaceCreateReq, WorkspaceCreateRes, WorkspaceDestroyReq, WorkspaceDestroyRes,
    WorkspaceListEntry, WorkspaceListReq, WorkspaceListRes,
};
// `is_private_dir` stays module-private to `session_socket` (not
// re-exported here): nothing outside that module calls it directly
// (`runtime_sot_dir` is its only caller), so a crate-root re-export would
// be dead weight -- codex follow-up, ADR 0042 L2b.
pub use session_socket::{current_uid, runtime_sot_dir, session_socket_path, slug};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Product version embedded at build time (ADR 0030 §1, §8 decision 31a):
/// `X.Y.Z` when the build sits exactly on its release tag `vX.Y.Z` with a
/// clean tree, `X.Y.Z-dev+<sha>` for an ordinary clean dev build, or
/// `X.Y.Z-dev+<sha>-dirty` when the working tree had uncommitted changes at
/// build time. Plain `X.Y.Z` when built without git (release tarballs). The
/// `-dev` marker is what gates the auto-updater — a dev build must never
/// self-update.
///
/// Invariant: a version string never claims to be a commit it was merely
/// built FROM — two builds that would otherwise print the same
/// `X.Y.Z-dev+<sha>` (one clean, one with local edits on top) are exactly
/// the pair a build-boundary gate (`sot_log::exchange::SUPERVISOR_LANE_BUILD_ID`)
/// refuses to pair, so their `app_version()` strings must differ too. A tree
/// on-tag but ALSO dirty is not the release it claims to sit on, so it falls
/// through to the `-dirty` form rather than collapsing to the bare tag
/// version. Two distinct dirty trees at one HEAD still alias on this sha —
/// deferred, same as `rust/log/build.rs` already documents for its own
/// build id.
pub fn app_version() -> String {
    format_app_version(
        env!("CARGO_PKG_VERSION"),
        env!("SOT_BUILD_SHA"),
        env!("SOT_BUILD_ON_TAG") == "1",
        env!("SOT_BUILD_DIRTY") == "1",
    )
}

/// The pure formatting core of [`app_version`], pulled out so the `-dirty`
/// shape is unit-tested without needing distinct compile-time `env!` values
/// (those are baked in once per build, so `app_version()` itself can only
/// ever exercise ONE branch per test run).
fn format_app_version(pkg: &str, sha: &str, on_tag: bool, dirty: bool) -> String {
    if sha.is_empty() {
        return pkg.to_string();
    }
    if on_tag && !dirty {
        pkg.to_string()
    } else if dirty {
        format!("{pkg}-dev+{sha}-dirty")
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

#[cfg(test)]
mod app_version_tests {
    use super::format_app_version;

    #[test]
    fn dirty_build_differs_from_clean_at_the_same_sha() {
        let clean = format_app_version("0.6.0", "abc1234", false, false);
        let dirty = format_app_version("0.6.0", "abc1234", false, true);
        assert_eq!(clean, "0.6.0-dev+abc1234");
        assert_eq!(dirty, "0.6.0-dev+abc1234-dirty");
        assert_ne!(clean, dirty, "a dirty build must never alias a clean one's version string");
    }

    #[test]
    fn dirty_on_tag_still_shows_dirty_not_the_bare_tag() {
        // A tree that sits ON the release tag but carries local edits is not
        // the release it claims to be — must not collapse to the bare
        // `X.Y.Z` a clean on-tag build reports.
        let v = format_app_version("0.6.0", "abc1234", true, true);
        assert_eq!(v, "0.6.0-dev+abc1234-dirty");
    }

    #[test]
    fn clean_on_tag_is_the_bare_version() {
        let v = format_app_version("0.6.0", "abc1234", true, false);
        assert_eq!(v, "0.6.0");
    }

    #[test]
    fn no_git_is_the_bare_version_regardless_of_flags() {
        // Release tarball: no sha to qualify, so on_tag/dirty go unread.
        assert_eq!(format_app_version("0.6.0", "", true, true), "0.6.0");
        assert_eq!(format_app_version("0.6.0", "", false, false), "0.6.0");
    }
}
