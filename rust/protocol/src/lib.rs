// sot-protocol
//
// Shared types for the JSON line protocol between frontend, backend, and kernel.
// Wire format: NDJSON envelopes — one JSON object per line, UTF-8, `\n`-terminated.
// Blob payloads are length-prefixed binary frames following an envelope whose
// payload contains `"blob": {"len": N, "mime": "…"}`. See docs/adr/0001.

pub mod annotation;
pub mod codec;
pub mod ir;
pub mod ops;
pub mod page_url;
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
    FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, GpuSample, HANDOFF_ROLE, HelloReq, HelloRes,
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
// `is_private_dir` stays module-private to `topology::endpoint` (not
// re-exported here): nothing outside that module calls it directly
// (`runtime_sot_dir` is its only caller), so a crate-root re-export would
// be dead weight -- codex follow-up, ADR 0042 L2b.
pub use topology::endpoint::{current_uid, local_daemon_label, runtime_sot_dir, session_socket_path, slug};

use serde::{Deserialize, Serialize};

/// The wire protocol a hello must speak; the daemon's gate refuses any other with `protocol_mismatch`. 3 (ADR
/// 0049 `## User isolation`): every hello declares the host and the OS account it runs as, and a connection
/// that becomes a pipe or a lease says so in its hello. Every pre-0.6.6 client speaks 2 and so meets the gate.
/// The shell client's hello carries the same number by hand; `the_shell_hello_is_this_protocols_hello` checks it.
pub const PROTOCOL_VERSION: u32 = 3;

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

#[cfg(test)]
mod client_wire_tests {
    use std::process::Command;

    /// The bash a shell client runs under: Git Bash on Windows, `bash` elsewhere. `None` only where Git Bash is
    /// missing and no CI is running; in CI a missing bash fails the test.
    fn bash() -> Option<std::path::PathBuf> {
        if !cfg!(windows) {
            return Some("bash".into());
        }
        let found = std::env::var_os("ProgramFiles")
            .map(|p| std::path::PathBuf::from(p).join("Git").join("bin").join("bash.exe"))
            .filter(|p| p.exists());
        assert!(found.is_some() || std::env::var_os("CI").is_none(), "CI has no Git Bash at %ProgramFiles%\\Git\\bin\\bash.exe");
        found
    }

    /// The shell client's hello as its builder, `sot_hello_frame`, prints it: one request frame whose payload is a
    /// `hello` at this `PROTOCOL_VERSION`, naming the host and this process's OS account, with no token. The builder
    /// writes its `"protocol"` by hand, so this run is what holds it to `PROTOCOL_VERSION`.
    #[test]
    fn the_shell_hello_is_this_protocols_hello() {
        let Some(bash) = bash() else { return };
        let lib =
            format!("{}/../../comm/lib/comm-lib.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
        let home = tempfile::tempdir().expect("scratch home");
        let out = Command::new(bash)
            .args(["-c", &format!(". '{lib}' && sot_hello_frame")])
            .env("HOME", home.path())
            .env("SOT_COMM_HOME", home.path().join(".sot-comm"))
            .env("SOT_SELF_HOST", "hello-test-host")
            .env_remove("SOT_WORKSPACE")
            .output()
            .expect("run bash");
        assert!(
            out.status.success(),
            "sot_hello_frame failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let frame: super::Frame =
            serde_json::from_slice(&out.stdout).expect("the shell hello is one JSON frame");
        assert_eq!(
            (frame.kind, frame.op.as_str()),
            (super::Kind::Req, "hello"),
            "the shell hello's envelope"
        );
        assert!(
            frame.payload.get("token").is_none(),
            "the shell client's hello sends no token: {}",
            frame.payload
        );
        let hello: super::ops::HelloReq =
            serde_json::from_value(frame.payload).expect("its payload is a hello request");
        assert_eq!(
            hello.protocol,
            super::PROTOCOL_VERSION,
            "the shell client's hello protocol"
        );
        assert_eq!(
            hello.host.as_deref(),
            Some("hello-test-host"),
            "the shell client's hello host"
        );
        let own = sot_log::identity::os_account::own_account_id()
            .expect("the OS issues this process's account");
        assert_eq!(
            hello.os_user.as_deref(),
            Some(own.as_str()),
            "the shell client's hello account"
        );
    }

    /// ADR 0049 `## User isolation`: a hello names the OS account its process runs as, and two accounts on one
    /// host are told apart by that string, so every client must write the same one. The Rust builders use
    /// `own_account_id()`; the shell client reads `_sot_os_user`, the launcher's PowerShell the process token's
    /// user SID. This runs the other two and holds each to the first, on whichever OS runs the test.
    #[test]
    fn every_client_declares_the_same_account() {
        let own = sot_log::identity::os_account::own_account_id().expect("the OS issues this process's account");
        if let Some(bash) = bash() {
            let lib = format!("{}/../../comm/lib/comm-lib.sh", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
            let scratch = std::env::temp_dir().join(format!("sot-protocol-os-user-{}", std::process::id()));
            std::fs::create_dir_all(&scratch).expect("scratch home");
            let out = Command::new(bash)
                .args(["-c", &format!(". '{lib}' && _sot_os_user")])
                .env("HOME", &scratch)
                .env("SOT_COMM_HOME", scratch.join(".sot-comm"))
                .output()
                .expect("run bash");
            let _ = std::fs::remove_dir_all(&scratch);
            assert!(out.status.success(), "_sot_os_user failed: {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), own, "the shell client's account");
        }
        #[cfg(windows)]
        {
            let out = Command::new("powershell")
                .args(["-NoProfile", "-Command", "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value"])
                .output()
                .expect("run powershell");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), own, "the launcher's account");
        }
    }
}
