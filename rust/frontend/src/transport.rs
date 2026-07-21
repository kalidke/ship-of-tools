// transport.rs — local frontend ↔ remote backend over a local socket (Unix
// socket / Windows named pipe) or TCP.
//
// Per ADR 0010:
//   - Backend listens on a per-session Unix socket on the remote
//     ($XDG_RUNTIME_DIR/sot/<session_id>.sock), inside a tmux session.
//   - Frontend spawns/attaches an SSH connection that forwards that socket to
//     a local Unix socket via:
//         ssh -o ExitOnForwardFailure=yes
//             -o ServerAliveInterval=15
//             -o StreamLocalBindUnlink=yes
//             -L "$LOCAL_SOCK:$REMOTE_SOCK"
//             <remote>
//   - Where a native frontend wants a local TCP endpoint (notably Windows),
//     the launcher forwards that local TCP port to the remote Unix socket:
//         ssh -L "<local-port>:$REMOTE_SOCK" <remote>
//     The frontend still connects with `--tcp 127.0.0.1:<local-port>`, but
//     the remote endpoint is scoped by the SSH user and socket permissions.
//     A direct backend `--tcp` listener remains an explicit fallback and is
//     token-gated.
//   - Connect handshake carries (session_id, client_id, last_seen_revision);
//     backend either replays missed events or sends a snapshot on reconnect.
//
// Transport selection: `spawn` takes both pipe and tcp options. If both are
// set, the pipe is tried first; on connect failure we log a warn and fall
// back to TCP. The protocol code is generic over `AsyncRead` / `AsyncWrite`
// so it runs identically on either transport.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath,
};
use serde_json::Value;
use sot_protocol::{
    codec, op, ConceptReadReq, ConceptReadRes, ConceptWriteReq, ConceptWriteRes, DocsOpenReq,
    DocsOpenRes, FileChunk, FileDeleteReq, FileDeleteRes, FileDownloadReq, FileReadReq,
    FileReadRes, FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, Frame, HelloReq,
    HelloRes, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    MonitorHistoryReq, MonitorHistoryRes, MonitorSubscribeRes, MonitorTickEvt, PlutoOpenReq,
    PlutoOpenRes, PreviewGetReq, PreviewGetRes, PtyOpenReq, PtyOpenRes, PtyResizeReq, PtyScrollReq,
    PtyWriteReq,
    QuartoOpenReq, QuartoOpenRes, ReplEvalReq, ReplEvalRes, ReplFrame, ReplFrameEvt,
    ReplRunFileReq, ReplRunFileRes, TmuxCapturePaneReq, TmuxCapturePaneRes, TmuxCreateSessionReq,
    TmuxKillSessionReq, TmuxListPanesReq, TmuxListPanesRes, TmuxListSessionsRes, TmuxPane,
    TmuxSession, ToggleHiddenReq, TreeChildrenReq, TreeChildrenRes, TreeNode, TreeRootReq,
    TreeRootRes, VideoOpenReq, VideoOpenRes, WorkspaceListReq, WorkspaceListRes,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};

use winit::window::Window;

/// What the transport task should dial. At least one of `pipe`/`tcp` must be
/// set or `spawn` is a no-op (caller checks).
#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub pipe: Option<PathBuf>,
    pub tcp: Option<String>,
    pub token: Option<String>,
}

/// Messages the transport task pushes back to the GPU thread.
#[derive(Debug)]
pub enum IncomingEvt {
    Connected {
        session_id: String,
        revision: u64,
        /// Hostname the backend reported via `gethostname` in HelloRes —
        /// `Some("myhost")` when a remote backend reports itself, `None`
        /// for older backends.
        host: Option<String>,
        /// `--project-root` the backend was started with, so the chrome
        /// can show "myhost:Ship of Tools" rather than just the host.
        project_root: Option<String>,
    },
    Disconnected {
        reason: String,
    },
    /// The backend refused the handshake because the FE↔BE wire-contract
    /// protocol versions differ (ADR 0030 §2). Unlike a transient
    /// `Disconnected`, this is a hard, self-diagnosing skew: the chrome shows a
    /// persistent blocking full-pane "update needed" message carrying both
    /// sides' versions + protocols + the dev fix hint. `message` is the
    /// pre-formatted multi-line body to display.
    ProtocolMismatch {
        message: String,
    },
    /// A `file.download` chunk landed and the transport wrote it to `dest`.
    /// `written` is the cumulative byte count so far (offset + this chunk),
    /// `total` the full size; `eof` marks the final chunk so the chrome can
    /// flip the status line to "downloaded".
    FileDownloadProgress {
        dest: PathBuf,
        written: u64,
        total: u64,
        eof: bool,
    },
    /// A `file.upload` chunk was acked by the backend. The chrome drives flow
    /// control: on each non-`done` ack it reads + sends the next chunk. On the
    /// `done` ack, `final_name` is the basename actually written (post
    /// sanitize + ` (1)` de-dup).
    FileUploadAck {
        offset: u64,
        done: bool,
        final_name: Option<String>,
    },
    /// A file transfer aborted. `op` is `"download"` or `"upload"` (for the
    /// status line); `message` is the backend error or a local I/O failure.
    FileTransferFailed {
        op: &'static str,
        message: String,
    },
    /// `image.crop` succeeded (ADR 0022): the backend wrote the ROI PNG at
    /// `path` (on the backend host). The chrome pastes a "look at this" line
    /// referencing `path` into the LLM pane. `x,y,w,h` are the clamped crop
    /// rect; `src_w,src_h` the source image dims — surfaced in the paste.
    ImageCropped {
        node_id: String,
        path: String,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        src_w: u32,
        src_h: u32,
    },
    /// `image.crop` failed (bad node, non-image, decode/IO error).
    ImageCropFailed {
        node_id: String,
        message: String,
    },
    /// `preview.set_scale` failed (ADR 0034 live entry). The backend rejects
    /// with a code — `not_a_raster`, `bad_scale`, `path_escape`, `io_error`,
    /// `unknown_workspace`, `bad_node_id`, `not_a_file`,
    /// `files_mode_init_failed` — and the chrome surfaces it on the status
    /// line. Without this the prompt's "saving…" would hang forever on any
    /// rejection, which reads as a silent failure.
    ScaleSetFailed {
        node_id: String,
        message: String,
    },
    TreeRoot {
        /// ADR 0014: the workspace this root was requested for, echoed from
        /// the pending entry. Lets the chrome drop a stale reply (e.g. an
        /// in-flight tree.root from before a workspace switch, or the
        /// connect-time default fetch) instead of clobbering the now-active
        /// workspace's tree. `None` = default workspace.
        workspace_id: Option<String>,
        root: TreeNode,
        children: Vec<TreeNode>,
    },
    /// Children for the node the GPU thread asked to expand. `parent_id`
    /// echoes the request so the tree view can splice the children under the
    /// correct row even if multiple expansions are in flight.
    TreeChildren {
        /// ADR 0014: same workspace guard as `TreeRoot` — a lazy-expand
        /// reply for a workspace we've since left must not splice into the
        /// current workspace's tree.
        workspace_id: Option<String>,
        parent_id: String,
        children: Vec<TreeNode>,
    },
    /// A `tree.children` request came back as an ERROR frame (or failed to
    /// parse). Previously this was warn-and-drop, which silently starved any
    /// deep-path reveal awaiting that parent (2026-07-10 symlink-reveal
    /// diagnosis); now the GPU thread gets told so it can abort the reveal
    /// with a visible trace + status line.
    TreeChildrenFailed {
        /// Workspace the failed expand was fired for (from the pending
        /// entry). The chrome key-gates its reveal-abort on this so a
        /// failure for a PARKED workspace's expand can't abort the ACTIVE
        /// workspace's reveal that merely shares a `parent_id` string.
        workspace_id: Option<String>,
        parent_id: String,
        error: String,
    },
    /// The kernel reported its currently-loaded module list. The chrome
    /// turns each name into a synthetic `TreeNode` and feeds them through
    /// the same `TreeView::set_root` path Files-mode uses. `path` is
    /// `Some(file)` when `Base.pathof(mod)` returned a value (per Linux's
    /// `4e1c8c0`) — built-ins like Base/Core have `None` and aren't
    /// expandable into col-2 definitions.
    ModulesList {
        /// The workspace this list was requested for, echoed from the pending
        /// entry (tree-provenance redesign — lets the chrome install into the
        /// right (Modules, workspace) slot instead of blindly into the shared
        /// tree). `None` = default workspace.
        workspace_id: Option<String>,
        modules: Vec<ModuleInfo>,
    },
    /// `kernel.request project.scan` reply — full nested package tree
    /// (modules → types/functions/submodules). Drives the unified
    /// Modules+Types nav mode; surfaces from a single round-trip
    /// rather than module-by-module `file.parse` calls.
    ProjectScan {
        /// Same ws echo as `ModulesList` (tree-provenance redesign).
        workspace_id: Option<String>,
        /// Absolute path of the workspace's `project_root`. The chrome
        /// needs this to strip the prefix off the absolute file paths
        /// each entry carries before firing `preview.get` (which takes
        /// a `files:<relpath>` node id).
        project_root: Option<String>,
        package_name: Option<String>,
        entry_file: Option<String>,
        modules: Vec<ScanModule>,
    },
    /// `concept.read` reply for `target`. `content` is the raw markdown
    /// (including YAML frontmatter) if `exists`; empty otherwise. Used by
    /// the chrome to show the annotation under the selected tree node and
    /// to drive the drift badge once `synced_against`-vs-AST-hash compare
    /// lands.
    #[allow(dead_code)] // chrome consumer lands in the next commit
    ConceptRead {
        target: String,
        exists: bool,
        content: String,
    },
    /// `concept.write` reply for `target`. `result` distinguishes the
    /// happy path from the stale-write optimistic-concurrency refusal
    /// (Linux's `4ebca35`) and from any other backend error so the chrome
    /// can offer the right next-step UX. Consumer is the concept-write
    /// editor in gpu.rs.
    ConceptWriteDone {
        target: String,
        result: ConceptWriteResult,
    },
    /// `file.read` reply — a source file's full text + content `version` for
    /// the editor (distinct from `preview.get`, which is kernel-rendered).
    #[allow(dead_code)] // editor consumer (gpu.rs) lands in the next commit
    FileRead {
        node_id: String,
        exists: bool,
        content: String,
        version: String,
    },
    /// `file.write` reply: happy path, optimistic-concurrency conflict, or
    /// error — see `FileWriteResult`.
    #[allow(dead_code)] // editor consumer (gpu.rs) lands in the next commit
    FileWriteDone {
        node_id: String,
        result: FileWriteResult,
    },
    /// `file.delete` reply (FE Ctrl+D): happy path or an `{error, code}`
    /// failure — see `FileDeleteResult`. The chrome matches `node_id` against
    /// the pending-delete id, refreshes the parent dir, and surfaces the
    /// trash location on success.
    FileDeleteDone {
        node_id: String,
        result: FileDeleteResult,
    },
    /// `kernel.request file.parse` reply for `path`. `ast_hash` is the
    /// SHA-256 of raw file bytes per ADR 0005 — the value the frontend
    /// compares against the annotation's `synced_against` frontmatter
    /// field to render the drift badge. `definitions` carries the
    /// `name/kind/line/parent/ast_hash` entries for top-level items in
    /// the file, used by Modules-mode col 2.
    FileParsed {
        /// Workspace the parse was fired for (tree-provenance redesign):
        /// keys the Modules col-2 splice so two workspaces defining the
        /// same module name can't cross-splice definitions.
        workspace_id: Option<String>,
        path: String,
        ast_hash: String,
        definitions: Vec<DefinitionInfo>,
    },
    /// `file.parse` came back without an `ast_hash` (kernel spawn failed,
    /// file unreadable, kernel.request error). The chrome un-latches the
    /// one-shot fire guard so the drift check retries on a later cursor
    /// pass instead of wedging at "checking…" for the whole session.
    FileParseFailed {
        /// Workspace the parse was fired for — the failure twin of
        /// `FileParsed.workspace_id`: the retry counter is keyed by
        /// workspace-RELATIVE path, so an ungated cross-workspace failure
        /// (both projects have a `src/lib.jl`) would advance the ACTIVE
        /// workspace's backoff for a parse it never fired (codex r3).
        workspace_id: Option<String>,
        path: String,
    },
    /// `kernel.request function.methods` reply for `module::name`. Methods
    /// become Modules-mode col-3 children of the function row.
    FunctionMethodsReceived {
        /// Same ws echo as `FileParsed` — keys the col-3 splice.
        workspace_id: Option<String>,
        module: String,
        name: String,
        methods: Vec<MethodInfo>,
    },
    Preview {
        /// Node id the response answers — the chrome uses this to
        /// resolve relative figure URLs in markdown previews against
        /// the markdown file's own directory. `None` for the connect-
        /// time root fetch (no PendingKind stamp to source from).
        node_id: Option<String>,
        /// Workspace the response came out of. Echoed back so figure
        /// fetches the chrome dispatches from this markdown go to the
        /// same workspace; otherwise a session whose
        /// `active_workspace_id` differs from the markdown's would
        /// look up the figure in the wrong project.
        workspace_id: Option<String>,
        mime: String,
        bytes: Vec<u8>,
        /// Plugin-reported metadata from `PreviewGetRes.extras` (ADR 0021).
        /// The chrome reads `page` / `page_count` to drive page-turn keys;
        /// unknown keys are ignored.
        extras: Option<serde_json::Value>,
    },
    /// A `figure.get` (`preview.get` op + figure-routed pending entry)
    /// reply: the bytes for a `![](url)` embedded in markdown. `url` is
    /// the original markdown URL — the chrome uses it as the cache key
    /// and to find which media-block this answers.
    FigureLoaded {
        url: String,
        mime: String,
        bytes: Vec<u8>,
    },
    /// A MathJax-rendered SVG blob arrived. Carries the `latex` and
    /// `display` flag from the originating `MathRender` request so the
    /// chrome can route it into its `(latex, display)`-keyed cache.
    /// The GPU thread rasterises the SVG and paints a quad over the
    /// FFFC placeholder the markdown walk emitted (per task A3 in
    /// phase-2).
    MathRendered {
        latex: String,
        svg_bytes: Vec<u8>,
        ex: f32,
        display: bool,
    },
    /// `markdown.tokenize` reply — backend-derived semantic spans for one
    /// fenced code block. Chrome keys the cache by `(lang, source_hash)`
    /// echoed from the outgoing request so a stale reply for a fence
    /// the user has since navigated away from still lands in the cache
    /// keyed by content (no race; the same fence in a later doc gets a
    /// cache hit). Byte ranges in `spans` are 0-indexed, end-exclusive.
    MarkdownTokens {
        lang: String,
        source_hash: u64,
        spans: Vec<MarkdownToken>,
    },
    /// A `repl.eval` reply arrived with the full frame list (synchronous-
    /// collect per ADR 0009; streamed frames are phase-2). `eval_id` echoes
    /// the request so the chrome can find the in-flight scrollback entry it
    /// pushed when the user hit Enter.
    ReplEvalDone {
        eval_id: u64,
        elapsed_ms: u64,
        frames: Vec<ReplFrame>,
    },
    /// One streamed REPL output frame (`repl.frame` evt, ADR 0009 phase-2),
    /// pushed as produced. `eval_id` correlates it to the in-flight scrollback
    /// entry; `workspace_id` to the originating workspace. The inner `Done`
    /// frame is terminal. Appended live; the `repl.eval`/`repl.run_file`
    /// response is now an empty-frames ack.
    ReplFrameStreamed {
        eval_id: u64,
        workspace_id: Option<String>,
        frame: ReplFrame,
    },
    /// Reply to a `pty.open` request — confirms the size the backend is
    /// running with. Frontend uses it to size its terminal emulator.
    PtyOpened {
        cols: u16,
        rows: u16,
        /// Foreground command of the (re-)targeted pane at attach time
        /// (`claude`/`node` ⇒ claude already running). Authoritative input to
        /// the autostart guard so it never relaunches into a live agent.
        /// `None` on a same-target resize or when the backend didn't probe.
        pane_command: Option<String>,
    },
    /// Bytes streamed out of the pty (`pty.evt`). The chrome feeds these
    /// into its `vt100::Parser`.
    PtyBytes {
        bytes: Vec<u8>,
    },
    /// Raw event we don't handle in the spike yet — kept for visibility.
    Event {
        op: String,
        #[allow(dead_code)]
        payload: Value,
    },
    /// Sessions mode (ADR 0013): `tmux.list_sessions` reply — every alive
    /// session on the backend's host tmux server (the same server the
    /// backend daemon itself lives in per ADR 0010).
    #[allow(dead_code)]
    TmuxSessions {
        sessions: Vec<TmuxSession>,
    },
    /// `tmux.list_panes` reply for the queried session (or for the whole
    /// server when `session: None` was sent).
    #[allow(dead_code)]
    TmuxPanes {
        /// Echoes the request scope; `None` when the request was server-wide.
        session: Option<String>,
        panes: Vec<TmuxPane>,
    },
    /// `tmux.create_session` reply — backend confirms it ran the spawn.
    /// Success carries `result: Ok(name)`; tmux-side failures land as
    /// `result: Err(msg)` and the UI surfaces a banner.
    #[allow(dead_code)]
    TmuxSessionCreated {
        result: Result<String, String>,
    },
    /// `tmux.kill_session` reply, same Ok/Err shape as create.
    #[allow(dead_code)]
    TmuxSessionKilled {
        result: Result<String, String>,
    },
    /// `tmux.capture_pane` reply: scrollback bytes for the requested target.
    /// Used by Sessions-mode col-3 live tail.
    #[allow(dead_code)]
    TmuxPaneCaptured {
        target: String,
        text: String,
    },
    /// `directory.list` reply for the workspace picker. `path` echoes
    /// the request so the chrome can route to the right tree node;
    /// `entries` is the immediate-subdirectory list rendered as picker
    /// rows.
    DirectoryList {
        path: String,
        entries: Vec<crate::transport::DirEntry>,
    },
    /// `workspace.create` reply: a workspace exists in the daemon and
    /// its tmux session is ready (when tmux didn't refuse). The chrome
    /// uses this to close the picker, refresh the Sessions list, and
    /// switch the active workspace to the new one.
    WorkspaceCreated {
        result: Result<WorkspaceCreatedInfo, String>,
    },
    /// `workspace.list` reply (ADR 0014). The Sessions-mode tree is
    /// rebuilt from this; each row carries label, project_root, the
    /// `kernel_running` badge, and an `is_default` flag.
    Workspaces {
        workspaces: Vec<WorkspaceInfo>,
    },
    /// `workspace.destroy` reply (ADR 0014). The chrome uses this to
    /// surface the result in the status line and re-fetch the
    /// workspace list so the destroyed row falls out.
    WorkspaceDestroyed {
        result: Result<WorkspaceDestroyedInfo, String>,
    },
    /// `pluto.open` reply. Carries the per-notebook edit URL on
    /// success; on failure carries the backend's error message so the
    /// chrome can surface it in the status line.
    PlutoOpened {
        result: Result<String, String>,
    },
    /// `video.open` reply. Carries the loopback HTTP URL on success (the
    /// chrome hands it to the OS browser-open); the backend's error message
    /// otherwise.
    VideoOpened {
        result: Result<String, String>,
    },
    /// `docs.open` reply. Carries the loopback HTTP URL of the built Documenter
    /// site on success (the chrome hands it to the OS browser-open); the
    /// backend's error message otherwise (e.g. docs not built). ADR 0024.
    DocsOpened {
        result: Result<String, String>,
    },
    /// `quarto.open` reply. Carries the rendered self-contained HTML bytes on
    /// success (the chrome writes a temp `.html` and OS-opens it, reusing the
    /// `text/html` preview path); the backend's error message otherwise.
    QuartoOpened {
        result: Result<Vec<u8>, String>,
    },
    /// `repl.run_file` reply. Surfaces priority J's `r` / `R` dispatch
    /// outcome. The REPL drawer's eval log already grew an entry via
    /// the frames-into-scrollback path; this evt drives the status line
    /// (success: "ran '<basename>' (fresh — project: <dir>)" /
    /// "ran '<basename>' (existing repl)"; error: the backend's message).
    /// The frames are *also* mirrored on this evt so the chrome can do
    /// future routing (e.g. TODO row 161: last-image → preview pane)
    /// without re-parsing the wire shape.
    ReplRunFileDone {
        /// Chrome-allocated id passed through from the request so the
        /// chrome can route response frames into the pre-registered
        /// `repl_log` entry even on the error path (where the backend's
        /// `{error, code}` envelope doesn't echo it).
        eval_id: u64,
        result: Result<ReplRunFileInfo, String>,
    },
    /// `monitor.subscribe` reply (ADR 0020): the cadence the backend will
    /// stream at (after clamping) and the host roster in display order, so
    /// the monitor drawer can lay out empty panels before the first tick.
    MonitorSubscribed {
        hosts: Vec<String>,
        #[allow(dead_code)] // ring sizing lands with the multiscale axis (Task 5)
        interval_s: f64,
    },
    /// `monitor.history` reply (ADR 0020): a downsampled window per host that
    /// prefills the monitor drawer's ring on open and on rescale.
    MonitorHistory {
        hosts: Vec<sot_protocol::HostSeries>,
    },
    /// `monitor.tick` evt (ADR 0020): one fresh sample per host at the
    /// subscribed cadence, appended to the live ring.
    MonitorTick {
        hosts: Vec<sot_protocol::HostLatest>,
    },
}

/// Success payload for `repl.run_file`. Carries the canonical fields the
/// chrome surfaces in the status line plus the frame list for any future
/// out-of-band routing (e.g. mirroring the last image frame to the
/// preview pane — TODO row 161).
#[derive(Debug, Clone)]
pub struct ReplRunFileInfo {
    pub eval_id: u64,
    pub path: String,
    pub fresh: bool,
    pub elapsed_ms: u64,
    pub project_dir: Option<String>,
    #[allow(dead_code)] // useful when frontend wants to distinguish
    // discovered vs fallback for status copy
    pub project_source: Option<String>,
    pub frames: Vec<ReplFrame>,
}

/// One row of the `workspace.list` response. Mirrors
/// `sot_protocol::WorkspaceListEntry` so the chrome can store it
/// without a protocol dependency on every consumer.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub tmux_session: String,
    pub kernel_running: bool,
    pub is_default: bool,
    /// Contract (b): the FE launches claude (ccb) on first attach to a
    /// workspace with `autostart_claude == true`. `agent_name` is the comm
    /// handle the bootstrap joins as (informational on the FE side).
    pub autostart_claude: bool,
    pub agent_name: String,
    /// Persisted spawn brief from the wire (mirrors the daemon's
    /// `WorkspaceListEntry.task`). The FE no longer delivers briefs (maintainer
    /// directive, 2026-06-16 — comm-spawn owns task delivery via a durable
    /// post-spawn comm message), so this is retained for protocol parity but
    /// intentionally unread on the FE side.
    #[allow(dead_code)]
    pub task: String,
    /// State-nav (ADR 0023 seam): the agent's work state read from the
    /// sot-comm registry by the daemon and copied onto the entry (the
    /// FE can't read the registry — separate machine/HOME). One of
    /// "working" | "idle" | "waiting" | "blocked" | "done"; "" when absent.
    pub agent_state: String,
    /// One-line glance of what the agent is doing / just did. "" when absent.
    pub agent_summary: String,
    /// ISO8601 (RFC3339) timestamp of the last state write — drives the
    /// staleness aging of a "working" that's gone quiet. "" when absent.
    pub agent_status_at: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceCreatedInfo {
    #[allow(dead_code)] // exposed by the protocol; frontend currently
    // keys off slug for active_workspace_id, but the
    // canonical id is what disk/IO consumers want
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub tmux_session: String,
}

/// `workspace.destroy` reply payload. `tmux_killed` and `toml_removed`
/// reflect what the daemon actually did — the chrome surfaces both in
/// the status line so the user can spot a half-success.
#[derive(Debug, Clone)]
pub struct WorkspaceDestroyedInfo {
    #[allow(dead_code)] // mirrors WorkspaceCreatedInfo; future routing
    // may need the canonical id even though slug is
    // what handlers key off today.
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub tmux_killed: bool,
    pub toml_removed: bool,
}

/// One row of the `directory.list` response, mirrored here so the
/// chrome doesn't have to depend on `sot_protocol::DirectoryEntry`
/// directly.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub has_children: bool,
}

/// Outcome of a `file.write` request, mirroring the backend's three response
/// shapes (success / optimistic-concurrency conflict / error).
#[derive(Debug, Clone)]
pub enum FileWriteResult {
    /// Write committed; `version` is the new content hash to keep editing against.
    #[allow(dead_code)]
    Ok { path: String, version: String },
    /// The on-disk file changed since the matching `FileRead`; carries the
    /// current content+version so the editor can reconcile, never auto-clobber.
    #[allow(dead_code)]
    Conflict {
        current_content: String,
        current_version: String,
    },
    /// Any other backend error — `code` is the protocol code, `message` detail.
    #[allow(dead_code)]
    Error { code: String, message: String },
}

/// Outcome of a `file.delete` request, mirroring the backend's two response
/// shapes (success / error). Directories are refused server-side with
/// `code: "is_directory"`, surfaced here as an `Error`.
#[derive(Debug, Clone)]
pub enum FileDeleteResult {
    /// File trashed; `path` is the absolute path that was removed and
    /// `trash_path` is the in-workspace recovery location when the
    /// `.sot-trash/` fallback was used (`None` for system trash).
    #[allow(dead_code)]
    Ok {
        path: String,
        trashed: bool,
        trash_path: Option<String>,
    },
    /// Any backend error — `code` is the protocol code (`bad_node_id`,
    /// `not_found`, `is_directory`, `file_delete_failed`, …), `message` detail.
    #[allow(dead_code)]
    Error { code: String, message: String },
}

/// Outcome of a `concept.write` request.
#[derive(Debug, Clone)]
pub enum ConceptWriteResult {
    /// Write committed; `path` is what the backend reported and
    /// `written` is the byte count. The chrome can clear the dirty
    /// flag and dismiss any save-in-flight indicator.
    // Fields are logged by the placeholder consumer; the full edit-mode
    // UI in the next commit will read them for status-line confirmation.
    #[allow(dead_code)]
    Ok { path: String, written: u64 },
    /// Optimistic-concurrency refusal: the on-disk `synced_against`
    /// no longer matches the `expected_ast_hash` we sent (someone else,
    /// or this client at an earlier session, wrote a newer version of
    /// the annotation). The chrome should surface a banner offering
    /// reload-discarding-edits vs keep-editing — never auto-clobber.
    Stale,
    /// Any other backend error — `code` is the protocol code (e.g.
    /// "io_error", "bad_request"), `message` the human-readable detail.
    /// Less common; chrome can show in a status line and let the user
    /// retry / discard.
    #[allow(dead_code)] // same — consumed in the next commit
    Error { code: String, message: String },
}

/// One row from `modules.list`. The kernel reply is a JSON object per
/// module; we extract just the fields the chrome consumes. `path` is
/// `None` for built-ins (Base, Core, Main) which have no on-disk file.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub path: Option<String>,
}

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

/// Requests the GPU thread asks the transport task to send. Kept narrow: only
/// the ops the interactive UI currently triggers. Adding a new op means a new
/// variant + a new arm in `handle_outgoing` and `handle_response`.
#[derive(Debug)]
pub enum OutgoingReq {
    TreeChildren {
        parent_id: String,
        /// ADR 0014: tags this request with a workspace so the backend
        /// routes to the right FilesMode. `None` = default workspace.
        workspace_id: Option<String>,
    },
    /// Re-request the root of a named mode tree. Used by the m/f mode-switch
    /// in the chrome to swap the left pane between modes. Today the backend
    /// only knows "files"; other modes are routed via `ModulesList` etc.
    #[allow(dead_code)] // constructed by the m/f keyboard handler in the
    // mode-switch commit; transport plumbing ships first.
    TreeRoot {
        mode: String,
        /// ADR 0014 workspace routing. Same shape as TreeChildren.
        workspace_id: Option<String>,
    },
    /// Flip the backend's per-workspace Files-mode "show hidden files" flag
    /// (`nav.toggle_hidden`, ADR: `.` keybind). The backend bumps
    /// `tree.invalidate`; the caller (gpu.rs) re-fetches `tree.root` right
    /// after on the same ordered connection so the new visibility shows up.
    /// The response carries the new state but the FE ignores it (the re-fetch
    /// is authoritative), so no PendingKind is stamped.
    ToggleHidden { workspace_id: Option<String> },
    /// Ask the kernel for its loaded-modules list. Response surfaces as
    /// `IncomingEvt::ModulesList`. Currently the only kernel.request the
    /// frontend issues directly; expand the enum as more land.
    #[allow(dead_code)] // ditto.
    ModulesList { workspace_id: Option<String> },
    /// Ask the kernel to scan the project's package source tree
    /// (`<project_root>/src/<PkgName>.jl` + everything it `include`s)
    /// and return a nested {modules → types/functions/submodules}
    /// view. Drives the unified Modules+Types nav mode.
    #[allow(dead_code)] // consumer lands in the unified-tree commit
    ProjectScan { workspace_id: Option<String> },
    /// Fetch the `.concept/<target>.md` annotation for `target`. Response
    /// surfaces as `IncomingEvt::ConceptRead`.
    #[allow(dead_code)] // chrome wires this up in the next commit
    ConceptRead {
        target: String,
        workspace_id: Option<String>,
    },
    /// Persist `content` as the `.concept/<target>.md` annotation. When
    /// `expected_ast_hash` is `Some`, the backend gates the write on
    /// the on-disk `synced_against` matching (Linux's `4ebca35`) and
    /// returns a `stale_write` error otherwise. The chrome should always
    /// send `expected_ast_hash` for an edit-then-save flow so the gate
    /// is engaged.
    #[allow(dead_code)] // edit-mode UI fires this in the next commit
    ConceptWrite {
        target: String,
        content: String,
        expected_ast_hash: Option<String>,
        workspace_id: Option<String>,
    },
    /// Read a source file's full text for the editor. Reply →
    /// `IncomingEvt::FileRead`. Distinct from `preview.get` (kernel-rendered).
    #[allow(dead_code)] // editor (gpu.rs) fires this in the next commit
    FileRead {
        node_id: String,
        workspace_id: Option<String>,
    },
    /// Persist editor content. `expected_version` (from the matching
    /// `FileRead`) engages the optimistic-concurrency gate; `None` forces.
    /// Reply → `IncomingEvt::FileWriteDone`.
    #[allow(dead_code)] // editor (gpu.rs) fires this in the next commit
    FileWrite {
        node_id: String,
        content: String,
        expected_version: Option<String>,
        workspace_id: Option<String>,
    },
    /// Trash a file from Files-mode nav (Ctrl+D). The backend refuses
    /// directories (`code: "is_directory"`); the FE pre-refuses them too so
    /// the prompt never opens on a dir row. Reply →
    /// `IncomingEvt::FileDeleteDone`.
    FileDelete {
        node_id: String,
        workspace_id: Option<String>,
    },
    /// Render LaTeX to a MathJax SVG. The chrome fires this once per
    /// distinct `(latex, display)` discovered in markdown previews;
    /// the response routes back via `IncomingEvt::MathRendered` and
    /// populates the chrome's per-key SVG cache. Backend's MathJax
    /// sidecar (`95f8176`) handles the rendering.
    MathRender { latex: String, display: bool },
    /// Crop an image node's visible ROI (source-image px) on the backend and
    /// write it to `<workspace>/.sot/captures/` as a PNG (ADR 0022). Reply
    /// → `IncomingEvt::ImageCropped`, which the chrome pastes into the LLM pane.
    ImageCrop {
        node_id: String,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        workspace_id: Option<String>,
    },
    /// Per-fence semantic-overlay highlighting via JuliaSyntax (or, in
    /// future, other backend-side parsers). Codex's industry-standard
    /// "tree-sitter base + LSP-style overlay" architecture: tree-sitter
    /// already paints keywords/strings/numbers/comments synchronously;
    /// the backend overlays things tree-sitter can't tell (function-def
    /// vs call-site, type annotation context, etc.). Response routes
    /// via `IncomingEvt::MarkdownTokens` and populates the per-fence
    /// cache keyed by `(lang, source_hash)`.
    MarkdownTokenize {
        lang: String,
        source_hash: u64,
        source: String,
    },
    /// Fetch the kernel's `file.parse` response for `path`. Used by the
    /// drift badge: the response's `ast_hash` is compared against the
    /// annotation's `synced_against` value.
    FileParse {
        path: String,
        workspace_id: Option<String>,
    },
    /// Fetch the methods of `module.name` via `kernel.request
    /// function.methods` (Linux's `b5faf94`). Modules-mode col-3
    /// expansion.
    FunctionMethods {
        module: String,
        name: String,
        workspace_id: Option<String>,
    },
    /// Fetch the preview for a tree node. Wired to cursor moves so the
    /// preview pane tracks navigation; the connect-time `preview.get`
    /// only seeds the initial pane content.
    PreviewGet {
        node_id: String,
        /// ADR 0014 workspace routing.
        workspace_id: Option<String>,
        /// 1-based page for paginated previews (ADR 0021). None = page 1.
        page: Option<u32>,
        /// Preview-pane px — render-fit hint for rasterizing plugins so
        /// pages arrive at display resolution (no resample aliasing).
        fit_w: Option<u32>,
        fit_h: Option<u32>,
    },
    /// Persist a user-entered physical scale for a raster and get the
    /// re-rendered preview back (ADR 0034 §4/§5 live entry).
    ///
    /// `nm_per_px` is the RAW/original pixel size the user typed (converted
    /// from µm at the prompt), sent verbatim: the backend writes exactly this
    /// to `<image>.scale.json` and returns the served-rescaled value for
    /// display, so a read-then-write round-trip can never compound the
    /// downsample ratio. The reply reuses the `PreviewGetRes` envelope and is
    /// routed through the ordinary preview path — one install path for a
    /// preview and its calibration, so the two can't drift.
    PreviewSetScale {
        node_id: String,
        /// Original-pixel nm/px, isotropic (a single typed value can't
        /// describe an anisotropic XZ view — that's Phase 3).
        nm_per_px: f64,
        workspace_id: Option<String>,
    },
    /// Fetch the bytes for a `![](url)` figure embedded in a markdown
    /// preview. Shares the `preview.get` wire op but stamps the response
    /// with `url` so the chrome routes it to the figure cache instead
    /// of replacing the active markdown buffer with the image.
    FigureGet {
        /// Literal URL string from the markdown source — also the
        /// chrome's figure-cache key.
        url: String,
        /// Resolved files-mode node id for the figure file. The chrome
        /// derives this from the current markdown file's directory + url.
        node_id: String,
        workspace_id: Option<String>,
    },
    /// Send a chunk of Julia code to the persistent REPL. `eval_id` is the
    /// chrome's per-eval counter; the response surfaces as
    /// `IncomingEvt::ReplEvalDone` carrying the same id so the in-flight
    /// scrollback entry can be reconciled.
    ReplEval {
        eval_id: u64,
        code: String,
        /// `"julia"` (default), `"pkg"` to route through the backend-side
        /// `Pkg.REPLMode.do_cmds` parser for `pkg>`-style commands.
        mode: Option<String>,
        /// ADR 0014 workspace routing — the backend dispatches the
        /// eval to the right per-workspace Repl handle.
        workspace_id: Option<String>,
    },
    /// Interrupt the workspace's currently-running REPL eval
    /// (`repl.interrupt`). Fire-and-forget: the backend schedules an
    /// `InterruptException` into the running eval task and the resulting
    /// error+done frames stream back to finalize the entry. No `eval_id` --
    /// the kernel interrupts its CURRENT_EVAL.
    ReplInterrupt { workspace_id: Option<String> },
    /// Open (or attach) the LLM-pane terminal at the given size.
    /// `target` selects the tmux session; `None` uses the historical
    /// `sot-llm`. Sessions mode (ADR 0013) passes a backend session
    /// name to re-target the BL pane.
    PtyOpen {
        cols: u16,
        rows: u16,
        target: Option<String>,
        /// True ONLY for an explicit user workspace-switch (the daemon
        /// re-targets the single foreground pty to a different session only
        /// then — the #5 guard). Background / reconnect / initial opens set
        /// false so they can't yank the foreground. See `PtyOpenReq`.
        user_switch: bool,
    },
    /// Resize an already-open pty (BL pane size changed).
    PtyResize { cols: u16, rows: u16 },
    /// Keystroke bytes to forward to the pty. Fire-and-forget — no
    /// response.
    PtyWrite { bytes: Vec<u8> },
    /// Keyboard PgUp/PgDn scrollback paging for the LLM pane — the backend
    /// drives tmux copy-mode (`op::PTY_SCROLL`). Fire-and-forget.
    PtyScroll { up: bool },
    /// Sessions-mode ops (ADR 0013 B1 backend; B2-B5 consumes here).
    /// All five round-trip through the host tmux server; responses surface
    /// as the matching `IncomingEvt::Tmux*` variants. Kept as
    /// `#[allow(dead_code)]` until Sessions-mode UI lands the call sites.
    #[allow(dead_code)]
    TmuxListSessions,
    #[allow(dead_code)]
    TmuxListPanes {
        /// `None` lists across the whole server; `Some(name)` scopes to one session.
        session: Option<String>,
    },
    #[allow(dead_code)]
    TmuxCreateSession {
        name: String,
        command: Option<String>,
        cwd: Option<String>,
    },
    #[allow(dead_code)]
    TmuxKillSession { name: String },
    #[allow(dead_code)]
    TmuxCapturePane { target: String, lines: u32 },
    /// List subdirectories of `path`. Used by the Sessions-mode workspace
    /// picker so the user can pick an existing directory as the new
    /// workspace's project_root.
    DirectoryList { path: String },
    /// Register a new workspace with the daemon and create its tmux
    /// session (ADR 0014). Fired when the user confirms a directory in
    /// the workspace picker. Response surfaces as
    /// `IncomingEvt::WorkspaceCreated`.
    WorkspaceCreate {
        label: String,
        project_root: String,
        /// Auto-start the comm-aware agent (ccb) in the new workspace's pane.
        /// `true` for a normal create (Enter); `false` for a bare session
        /// (Shift+Enter) — a plain shell/REPL with no LLM agent.
        autostart_claude: bool,
        /// ADR 0031 agent kind: "claude" | "codex" | "none".
        agent: String,
    },
    /// Enumerate registered workspaces on the daemon (ADR 0014).
    /// Replaces the `tmux.list_sessions` prefix-filter as the source of
    /// truth for Sessions mode rows. Response surfaces as
    /// `IncomingEvt::Workspaces` carrying the full registry view.
    WorkspaceList,
    /// Destroy a registered workspace (ADR 0014). Backend kills the
    /// tmux session, removes the toml, drops the in-memory entry.
    /// Default workspace is refused. Response surfaces as
    /// `IncomingEvt::WorkspaceDestroyed`.
    WorkspaceDestroy { workspace_id: String },
    /// Open a Pluto-flavored `.jl` notebook in the backend-supervised
    /// Pluto server. Path is the absolute backend-side path. Response
    /// surfaces as `IncomingEvt::PlutoOpened` carrying the per-notebook
    /// edit URL on success.
    PlutoOpen { path: String },
    /// Ask the backend for a browser URL for a video file (HTTP-served +
    /// SSH-forwarded). Response surfaces as `IncomingEvt::VideoOpened`.
    VideoOpen { path: String },
    /// Open the project's built Documenter site in the OS browser (HTTP-served
    /// from `docs/build` + SSH-forwarded). `path` is the cursored file's
    /// absolute backend path (deep-links to a built page when applicable, else
    /// the index). Response surfaces as `IncomingEvt::DocsOpened`. ADR 0024.
    DocsOpen { path: String },
    /// Render a Quarto/markdown doc on the backend and open it in the OS
    /// browser. `execute = false` (`o`) = fast no-execute render; `execute =
    /// true` (`O`) runs code chunks. Response surfaces as
    /// `IncomingEvt::QuartoOpened` carrying the HTML bytes.
    QuartoOpen { path: String, execute: bool },
    /// Run a `.jl` file in the workspace's persistent REPL. Priority J:
    /// `r` in NavTree maps to `fresh: true` (REPL reset into the file's
    /// closest-ancestor Project.toml then include), `R` to `fresh:
    /// false` (just include in the existing REPL). Response surfaces as
    /// `IncomingEvt::ReplRunFileDone`.
    ReplRunFile {
        /// Pre-allocated by the chrome (same counter as repl.eval) so the
        /// chrome can pre-register a `repl_log` entry before the request
        /// lands and route response frames into it by id, parallel to
        /// the eval flow.
        eval_id: u64,
        path: String,
        fresh: bool,
        workspace_id: Option<String>,
    },
    /// Download a backend-host file to a local path. `path` is the absolute
    /// backend path (read-only, unrestricted to project root — matches
    /// `preview.get` reach). `dest` is the resolved local destination; the
    /// transport task creates it on the first chunk and writes each streamed
    /// chunk at its offset until `eof`.
    FileDownload { path: String, dest: PathBuf },
    /// Upload one chunk of a local file to a backend directory. The chrome
    /// drives flow control: it sends chunk 0, then sends the next chunk only
    /// after the matching `FileUploadAck`. `dir` is the absolute backend dest
    /// dir (the cursored nav folder); `name` the picked file's basename (the
    /// backend sanitizes + de-dups). `bytes` is the ≤1 MiB chunk; the backend
    /// truncates on `offset == 0`, writes at `offset`, finalizes on `eof`.
    FileUpload {
        dir: String,
        name: String,
        offset: u64,
        total: u64,
        eof: bool,
        bytes: Vec<u8>,
    },
    /// Start this connection's live metrics stream (ADR 0020). Fired when the
    /// Ctrl+M monitor drawer opens. Response surfaces as
    /// `IncomingEvt::MonitorSubscribed` (cadence + host roster).
    MonitorSubscribe,
    /// Stop this connection's live metrics stream. Fired when the monitor
    /// drawer closes. Fire-and-forget — the backend acks with a bare `{}` we
    /// don't track.
    MonitorUnsubscribe,
    /// Fetch a downsampled window for one or all hosts (ADR 0020). Fired on
    /// monitor-drawer open (and later on rescale) to prefill the ring. Response
    /// surfaces as `IncomingEvt::MonitorHistory`.
    MonitorHistory {
        window_s: f64,
        points: u32,
        until: Option<f64>,
        host: Option<String>,
    },
}

/// Internal bookkeeping so a response frame can be deserialized as the right
/// type. Inserted when the writer sends a request; consumed when the matching
/// reply id arrives.
#[derive(Debug)]
enum PendingKind {
    TreeChildren {
        parent_id: String,
        workspace_id: Option<String>,
    },
    TreeRoot {
        workspace_id: Option<String>,
    },
    /// Tree-provenance redesign: both kernel-request tree loaders now CARRY
    /// the workspace they were fired for (previously discarded here, which
    /// left their replies un-keyable — a late Modules reply could clobber
    /// another workspace's tree with no way to detect it; the v0.4.3 saga's
    /// last open hole).
    ModulesList {
        workspace_id: Option<String>,
    },
    ProjectScan {
        workspace_id: Option<String>,
    },
    MarkdownTokenize {
        lang: String,
        source_hash: u64,
    },
    ConceptRead {
        target: String,
    },
    ConceptWrite {
        target: String,
    },
    FileRead {
        node_id: String,
    },
    FileWrite {
        node_id: String,
    },
    FileDelete {
        node_id: String,
    },
    MathRender {
        latex: String,
        display: bool,
    },
    ImageCrop {
        node_id: String,
    },
    FileParse {
        path: String,
        workspace_id: Option<String>,
    },
    FunctionMethods {
        module: String,
        name: String,
        workspace_id: Option<String>,
    },
    PreviewGet {
        node_id: String,
        workspace_id: Option<String>,
    },
    /// Reply to `preview.set_scale`. Carries the SAME `PreviewGetRes` envelope
    /// as a normal preview, so it decodes with the existing type and surfaces
    /// as `IncomingEvt::Preview` — the chrome installs it through the one
    /// preview path it already has (ADR 0034 §5).
    SetScale {
        node_id: String,
        workspace_id: Option<String>,
    },
    FigureGet {
        url: String,
    },
    ReplEval {
        eval_id: u64,
    },
    PtyOpen,
    TmuxListSessions,
    TmuxListPanes {
        session: Option<String>,
    },
    TmuxCreateSession,
    TmuxKillSession,
    TmuxCapturePane {
        target: String,
    },
    DirectoryList,
    WorkspaceCreate,
    WorkspaceList,
    WorkspaceDestroy,
    PlutoOpen,
    VideoOpen,
    DocsOpen,
    QuartoOpen,
    ReplRunFile {
        eval_id: u64,
        path: String,
        fresh: bool,
    },
    /// A `file.download` is streaming. The pending entry is re-inserted on
    /// each non-`eof` chunk (one request id, many response frames). `file` is
    /// lazily created on the first chunk so a backend error before any chunk
    /// leaves no empty file behind.
    FileDownload {
        dest: PathBuf,
        file: Option<std::fs::File>,
    },
    /// A `file.upload` chunk awaiting its ack. 1:1 — each chunk is its own
    /// request id, so no re-insert.
    FileUpload,
    /// A `monitor.subscribe` awaiting its cadence + roster reply (ADR 0020).
    MonitorSubscribe,
    /// A `monitor.history` awaiting its windowed per-host series (ADR 0020).
    MonitorHistory,
}

/// Create the outgoing-request channel paired with the transport task. The
/// sender lives on the GPU thread; the receiver gets handed to `spawn`. Both
/// sides drop their handle on shutdown — that's how the writer half of the
/// select loop terminates.
pub fn outgoing_channel() -> (UnboundedSender<OutgoingReq>, UnboundedReceiver<OutgoingReq>) {
    tmpsc::unbounded_channel()
}

/// Spawn the transport task on `rt`. Returns once spawned; the task runs
/// until the connection drops or the runtime shuts down. The task asks the
/// window to redraw whenever a new IncomingEvt is published so the GPU
/// thread sees state updates without polling.
pub fn spawn(
    rt: &tokio::runtime::Runtime,
    config: TransportConfig,
    evt_tx: StdSender<IncomingEvt>,
    out_rx: UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    reconnect_now: Arc<tokio::sync::Notify>,
) {
    rt.spawn(async move {
        // Reconnect loop with exponential backoff capped at 5s. The
        // out_rx channel survives across attempts; any OutgoingReq the
        // user queued while disconnected gets sent once the next
        // connection is up. Per ADR 0010 the backend's session-id +
        // last-seen-revision handshake on each connect carries the
        // resume protocol, so missed events replay automatically.
        //
        // We never give up — the user can quit the window to terminate
        // the task. Backoff resets to the floor after `connect_and_run`
        // reaches `hello_res` (signalling a real round-trip succeeded)
        // OR after a clean Ok return; mid-handshake failures keep
        // walking the backoff up so a thrashing backend doesn't get
        // hammered. The F5 `reconnect_now` notify lets the user
        // collapse the current sleep — useful when wifi flickers and
        // the user knows it's back before the 5s cap elapses.
        let mut out_rx = out_rx;
        let mut backoff_ms: u64 = 200;
        const BACKOFF_FLOOR_MS: u64 = 200;
        const BACKOFF_CAP_MS: u64 = 5_000;
        loop {
            match connect_and_run(
                config.clone(),
                evt_tx.clone(),
                &mut out_rx,
                window.clone(),
                &mut backoff_ms,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!("transport task exited cleanly");
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        backoff_ms,
                        "transport task ended; reconnecting"
                    );
                    let _ = evt_tx.send(IncomingEvt::Disconnected {
                        reason: format!("{e:#} — retry in {backoff_ms}ms (F5 to retry now)"),
                    });
                    window.request_redraw();
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                        _ = reconnect_now.notified() => {
                            tracing::info!("manual reconnect requested — collapsing backoff");
                            backoff_ms = BACKOFF_FLOOR_MS;
                            continue;
                        }
                    }
                    backoff_ms = (backoff_ms.saturating_mul(2)).min(BACKOFF_CAP_MS);
                }
            }
        }
    });
}

/// Try pipe first (if set), fall back to TCP on connect failure (if set).
/// Once a connection is established we hand off to `run_protocol`; any error
/// from there is *not* retried via the other transport — that's a runtime
/// disconnect, not a startup-time choose-your-transport decision.
async fn connect_and_run(
    config: TransportConfig,
    evt_tx: StdSender<IncomingEvt>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    backoff_ms: &mut u64,
) -> Result<()> {
    if let Some(pipe_path) = config.pipe.as_ref() {
        match connect_pipe(pipe_path).await {
            Ok(stream) => {
                tracing::info!(?pipe_path, "connected via local socket");
                let (rx, tx) = stream.split();
                let rx = codec::buffered(rx);
                return run_protocol(
                    rx,
                    tx,
                    config.token.as_deref(),
                    &evt_tx,
                    out_rx,
                    &window,
                    backoff_ms,
                )
                .await;
            }
            Err(e) if config.tcp.is_some() => {
                tracing::warn!(
                    pipe = ?pipe_path,
                    error = %e,
                    "local-socket connect failed; falling back to TCP"
                );
                // fall through to TCP block below
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(tcp_addr) = config.tcp.as_ref() {
        let stream = tokio::net::TcpStream::connect(tcp_addr)
            .await
            .with_context(|| format!("connect tcp {tcp_addr}"))?;
        // Disable Nagle so single-keystroke writes (typing into the LLM
        // pty pane) and the PTY-echo packets coming back don't get
        // coalesced up to the OS Nagle window. Without this, LAN-local
        // typing showed ~100ms latency on a <1ms RTT path. Backend
        // needs the same set_nodelay on its accept side to fully
        // disable Nagle in both directions.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!(error = %e, "set_nodelay failed on tcp stream");
        }
        tracing::info!(%tcp_addr, "connected via tcp");
        let (rx, tx) = stream.into_split();
        let rx = codec::buffered(rx);
        return run_protocol(
            rx,
            tx,
            config.token.as_deref(),
            &evt_tx,
            out_rx,
            &window,
            backoff_ms,
        )
        .await;
    }
    anyhow::bail!("no transport configured (set --socket or --tcp)")
}

/// Connect to the local socket / named pipe at `path`.
async fn connect_pipe(path: &std::path::Path) -> Result<LocalStream> {
    let path_str = path.to_str().context("socket path must be valid UTF-8")?;
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    LocalStream::connect(name)
        .await
        .with_context(|| format!("connect {path:?}"))
}

/// Read exactly one frame while *owning* the reader, handing it back with the
/// result. This lets the steady-state select! loop keep a single in-flight
/// read future across iterations (cancel-safe: a cancelled select! pauses it
/// rather than dropping it mid-blob) without the borrow checker objecting to a
/// stored future that re-borrows `rx` each loop. See the CANCEL-SAFETY note in
/// `run_protocol`'s steady-state loop.
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// Drive the wire protocol over an already-connected stream's halves. Generic
/// over the read/write types so the same code path serves the local-socket
/// transport and the TCP transport.
async fn run_protocol<R, W>(
    mut rx: tokio::io::BufReader<R>,
    mut tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<IncomingEvt>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Arc<Window>,
    backoff_ms: &mut u64,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, PendingKind> = HashMap::new();

    // Reconnect memory: client_id stays stable across runs; session_id +
    // last_seen_revision feed the backend's replay path. First-ever launch
    // produces fresh values and the backend assigns a session_id we'll
    // remember for next time.
    let mut memory = crate::state::load();
    tracing::info!(
        client_id = %memory.client_id,
        ?memory.session_id,
        last_seen_revision = memory.last_seen_revision,
        token_set = token.is_some(),
        "loaded session memory"
    );

    // hello
    let hello_id = take_id(&mut next_id);
    let hello = HelloReq {
        client_id: memory.client_id.clone(),
        session_id: memory.session_id.clone(),
        last_seen_revision: memory.last_seen_revision,
        token: token.map(|s| s.to_string()),
        // ADR 0030 §2: advertise our wire-contract protocol + product version
        // so the backend can gate on protocol equality and name both sides in
        // a mismatch error.
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
    };
    codec::write_frame(
        &mut tx,
        &Frame::req(hello_id, op::HELLO, serde_json::to_value(&hello)?),
        None,
    )
    .await?;

    let (frame, _) = codec::read_frame(&mut rx).await?;
    if frame.id != hello_id {
        anyhow::bail!("hello reply id mismatch: got {}, want {hello_id}", frame.id);
    }
    if let Some(r) = frame.rev {
        memory.last_seen_revision = memory.last_seen_revision.max(r);
    }
    // Inspect the frame for an error envelope first — the backend rejects
    // bad auth (and any other hello-time refusal) with `{error, code}`,
    // which does not deserialize as HelloRes. Surfacing it as a clear
    // auth-failed message beats a `serde_json` "hello res" error.
    if let Some(err_msg) = frame.payload.get("error").and_then(|v| v.as_str()) {
        let code = frame
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if code == "token_mismatch" {
            tracing::error!(code, "hello rejected: authentication failed ({err_msg})");
            anyhow::bail!("authentication failed: {err_msg}");
        }
        if code == "protocol_mismatch" {
            // ADR 0030 §2: a version skew, not a transient drop. Build a
            // readable multi-line body from the backend's structured fields
            // (falling back to its already-formatted `error` string) and add
            // the dev fix hint, then push it as a ProtocolMismatch evt so the
            // chrome shows a persistent blocking "update needed" screen. We
            // still bail afterward so the reconnect loop keeps the socket warm
            // — a re-hello re-affirms the same overlay, idempotently, until one
            // side is updated.
            let p = &frame.payload;
            let get_str = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let get_u32 = |k: &str| p.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let backend_version = {
                let v = get_str("backend_version");
                if v.is_empty() {
                    "<unknown>".to_string()
                } else {
                    v.to_string()
                }
            };
            let frontend_version = sot_protocol::app_version();
            let message = format!(
                "Update needed — FE/BE protocol mismatch\n\n\
                 backend:  {}  (protocol {})\n\
                 frontend: {}  (protocol {})\n\n\
                 dev: git pull + rebuild + relaunch · see docs/adr/0030\n\n\
                 ({err_msg})",
                backend_version,
                get_u32("backend_protocol"),
                frontend_version,
                sot_protocol::PROTOCOL_VERSION,
            );
            tracing::error!(code, "hello rejected: {err_msg}");
            let _ = evt_tx.send(IncomingEvt::ProtocolMismatch { message });
            window.request_redraw();
            anyhow::bail!("hello rejected: {err_msg} (code={code})");
        }
        tracing::error!(code, "hello rejected: {err_msg}");
        anyhow::bail!("hello rejected: {err_msg} (code={code})");
    }
    let hello_res: HelloRes = serde_json::from_value(frame.payload).context("hello res")?;
    // ADR 0030 §2: a successful hello from a pre-versioning backend comes back
    // with protocol == 0. We still run (it spoke a compatible wire), but warn
    // loudly so the skew is visible in logs — the backend should be updated.
    if hello_res.protocol == 0 {
        tracing::warn!(
            backend_version = %hello_res.app_version,
            "connected to a pre-versioning backend (protocol 0) — update the backend (ADR 0030)"
        );
    }
    if memory.session_id.as_deref() != Some(hello_res.session_id.as_str()) {
        // First run, or backend restarted with a new session — record the
        // assigned id so the next reconnect is on the live session.
        memory.session_id = Some(hello_res.session_id.clone());
    }
    crate::state::save(&memory).ok();
    // Hello round-trip succeeded — reset backoff to the floor so any
    // *future* disconnect in this session restarts the exponential
    // climb from 200ms rather than picking up wherever the previous
    // unrelated reconnect cycle left it (e.g. wifi flicker followed
    // by months of stable session).
    *backoff_ms = 200;
    let _ = evt_tx.send(IncomingEvt::Connected {
        session_id: hello_res.session_id.clone(),
        revision: hello_res.revision,
        host: hello_res.host.clone(),
        project_root: hello_res.project_root.clone(),
    });
    window.request_redraw();
    tracing::info!(
        session_id = %hello_res.session_id,
        revision = hello_res.revision,
        snapshot_pending = hello_res.snapshot_pending,
        "connected"
    );

    // tree.root — initial fetch on connect uses the default workspace
    // (no workspace_id). Once the chrome resumes a saved Sessions-mode
    // active_workspace_id it will re-fire this with the id set.
    let tree_id = take_id(&mut next_id);
    codec::write_frame(
        &mut tx,
        &Frame::req(
            tree_id,
            op::TREE_ROOT,
            serde_json::to_value(TreeRootReq {
                mode: "files".into(),
                workspace_id: None,
            })?,
        ),
        None,
    )
    .await?;
    let (frame, _) = codec::read_frame(&mut rx).await?;
    if let Some(r) = frame.rev {
        memory.last_seen_revision = memory.last_seen_revision.max(r);
        crate::state::save(&memory).ok();
    }
    // Hold the root node id so preview.get can target it without hardcoding
    // the backend's id-format conventions in the frontend. Today files mode
    // uses `files:` for the root; that may change.
    let mut root_node_id = String::new();
    if frame.id == tree_id {
        let res: TreeRootRes = serde_json::from_value(frame.payload).context("tree.root res")?;
        root_node_id = res.node.id.clone();
        let _ = evt_tx.send(IncomingEvt::TreeRoot {
            // Connect-time fetch is always the default workspace (see the
            // tree.root request above). If the chrome resumed a non-default
            // active workspace it re-fires tree.root with the id set, and
            // this default reply is dropped by the workspace check rather
            // than briefly flashing the wrong project's tree.
            workspace_id: None,
            root: res.node,
            children: res.children,
        });
        window.request_redraw();
    }

    // preview.get against whatever the backend just reported as the root.
    // For the spike that's enough to prove blob round-trip; real previews
    // follow user navigation.
    let prev_id = take_id(&mut next_id);
    codec::write_frame(
        &mut tx,
        &Frame::req(
            prev_id,
            op::PREVIEW_GET,
            serde_json::to_value(PreviewGetReq {
                node_id: root_node_id,
                workspace_id: None,
                page: None,
                fit_w: None,
                fit_h: None,
            })?,
        ),
        None,
    )
    .await?;
    let (frame, blob) = codec::read_frame(&mut rx).await?;
    if let Some(r) = frame.rev {
        memory.last_seen_revision = memory.last_seen_revision.max(r);
        crate::state::save(&memory).ok();
    }
    if frame.id == prev_id {
        let res: PreviewGetRes =
            serde_json::from_value(frame.payload).context("preview.get res")?;
        let bytes = blob.unwrap_or_default();
        let _ = evt_tx.send(IncomingEvt::Preview {
            node_id: None,
            workspace_id: None,
            mime: res.mime,
            bytes,
            extras: res.extras,
        });
        window.request_redraw();
    }

    // Steady-state loop. `tokio::select!` lets us simultaneously read frames
    // arriving from the backend (replays, future server-pushed evts, replies
    // to outgoing requests) and accept new requests from the GPU thread. The
    // request id is allocated on the writer side and stashed in `pending`;
    // the reader matches incoming response frames against it to route
    // deserialization. Unsolicited events (no id in pending) fall through to
    // the catch-all `Event` evt the same way the old idle loop handled them.
    //
    // CANCEL-SAFETY: `read_frame` is NOT cancellation-safe — it reads the
    // `\n`-terminated envelope and then `read_exact`s the blob tail across
    // two separate awaits. If we polled `codec::read_frame(&mut rx)` directly
    // as a select! arm, an outgoing request arriving while a blob was still
    // mid-flight would make select! drop the half-read future: the envelope
    // bytes were already consumed but the blob tail was not, so the next read
    // parsed leftover binary blob bytes as a JSON envelope, failed, and forced
    // a reconnect — the spurious-reconnect → tree-collapse → nav-reset bug.
    // Fix: hold one read future across iterations and poll it by `&mut`, so a
    // cancelled select! merely *pauses* it; it resumes mid-blob next iteration
    // instead of being recreated from a desynced stream offset. The future
    // *owns* the reader (via `read_owned`) and hands it back on completion, so
    // the borrow checker never sees an external `&mut rx` re-borrowed across
    // iterations.
    let mut read_fut = Some(Box::pin(read_owned(rx)));
    loop {
        tokio::select! {
            // Bias to reads so an avalanche of GPU-thread requests can't
            // starve replies. Spike-grade — revisit if it ever matters.
            biased;

            done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                // Completed: reclaim the reader and arm the next read.
                let (rx_back, read) = done;
                read_fut = Some(Box::pin(read_owned(rx_back)));
                let (frame, blob) = read?;
                if let Some(r) = frame.rev {
                    memory.last_seen_revision = memory.last_seen_revision.max(r);
                    crate::state::save(&memory).ok();
                }
                handle_response_frame(frame, blob, &mut pending, evt_tx);
                window.request_redraw();
            }

            req = out_rx.recv() => {
                let Some(req) = req else {
                    // Sender side dropped — the app is shutting down. Drain
                    // the reader by falling back to a plain read loop until
                    // the connection closes.
                    tracing::debug!("outgoing channel closed; draining reads until disconnect");
                    // Shutdown path: no more outgoing requests can race the
                    // reader, so cancel-safety no longer matters. Resume the
                    // in-flight read (reclaiming the reader), then fall back to
                    // plain sequential reads until the connection closes.
                    let fut = read_fut.take().expect("read_fut is always Some here");
                    let (mut rx, read) = fut.await;
                    let (frame, blob) = read?;
                    if let Some(r) = frame.rev {
                        memory.last_seen_revision = memory.last_seen_revision.max(r);
                        crate::state::save(&memory).ok();
                    }
                    handle_response_frame(frame, blob, &mut pending, evt_tx);
                    window.request_redraw();
                    loop {
                        let (frame, blob) = codec::read_frame(&mut rx).await?;
                        if let Some(r) = frame.rev {
                            memory.last_seen_revision = memory.last_seen_revision.max(r);
                            crate::state::save(&memory).ok();
                        }
                        handle_response_frame(frame, blob, &mut pending, evt_tx);
                        window.request_redraw();
                    }
                };
                let id = take_id(&mut next_id);
                match req {
                    OutgoingReq::TreeChildren { parent_id, workspace_id } => {
                        tracing::debug!(%parent_id, ?workspace_id, id, "→ tree.children");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TREE_CHILDREN,
                                serde_json::to_value(TreeChildrenReq {
                                    node_id: parent_id.clone(),
                                    workspace_id: workspace_id.clone(),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(
                            id,
                            PendingKind::TreeChildren { parent_id, workspace_id },
                        );
                    }
                    OutgoingReq::TreeRoot { mode, workspace_id } => {
                        tracing::debug!(%mode, ?workspace_id, id, "→ tree.root");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TREE_ROOT,
                                serde_json::to_value(TreeRootReq {
                                    mode,
                                    workspace_id: workspace_id.clone(),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TreeRoot { workspace_id });
                    }
                    OutgoingReq::ToggleHidden { workspace_id } => {
                        tracing::debug!(?workspace_id, id, "→ nav.toggle_hidden");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::NAV_TOGGLE_HIDDEN,
                                serde_json::to_value(ToggleHiddenReq {
                                    workspace_id,
                                    mode: Some("files".to_string()),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        // No PendingKind: the response's new-state is redundant
                        // with the tree.root re-fetch gpu.rs fires right after,
                        // and an unmatched response id is silently ignored.
                    }
                    OutgoingReq::ModulesList { workspace_id } => {
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
                    }
                    OutgoingReq::ProjectScan { workspace_id } => {
                        tracing::debug!(?workspace_id, id, "→ kernel.request project.scan");
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
                        pending.insert(id, PendingKind::ProjectScan { workspace_id });
                    }
                    OutgoingReq::MarkdownTokenize { lang, source_hash, source } => {
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
                    }
                    OutgoingReq::ConceptRead { target, workspace_id } => {
                        tracing::debug!(%target, ?workspace_id, id, "→ concept.read");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::CONCEPT_READ,
                                serde_json::to_value(ConceptReadReq {
                                    target: target.clone(),
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::ConceptRead { target });
                    }
                    OutgoingReq::MathRender { latex, display } => {
                        let is_display = display;
                        tracing::debug!(
                            latex_len = latex.len(),
                            is_display,
                            id,
                            "→ math.render"
                        );
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::MATH_RENDER,
                                serde_json::to_value(MathRenderReq {
                                    latex: latex.clone(),
                                    display,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::MathRender { latex, display });
                    }
                    OutgoingReq::ImageCrop { node_id, x, y, w, h, workspace_id } => {
                        tracing::debug!(%node_id, x, y, w, h, ?workspace_id, id, "→ image.crop");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::IMAGE_CROP,
                                serde_json::to_value(ImageCropReq {
                                    node_id: node_id.clone(),
                                    x,
                                    y,
                                    w,
                                    h,
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::ImageCrop { node_id });
                    }
                    OutgoingReq::ConceptWrite { target, content, expected_ast_hash, workspace_id } => {
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
                    }
                    OutgoingReq::FileRead { node_id, workspace_id } => {
                        tracing::debug!(%node_id, ?workspace_id, id, "→ file.read");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::FILE_READ,
                                serde_json::to_value(FileReadReq {
                                    node_id: node_id.clone(),
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FileRead { node_id });
                    }
                    OutgoingReq::FileWrite { node_id, content, expected_version, workspace_id } => {
                        tracing::debug!(
                            %node_id,
                            bytes = content.len(),
                            version = ?expected_version,
                            ?workspace_id,
                            id,
                            "→ file.write"
                        );
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::FILE_WRITE,
                                serde_json::to_value(FileWriteReq {
                                    node_id: node_id.clone(),
                                    content,
                                    expected_version,
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FileWrite { node_id });
                    }
                    OutgoingReq::FileDelete { node_id, workspace_id } => {
                        tracing::debug!(
                            %node_id,
                            ?workspace_id,
                            id,
                            "→ file.delete"
                        );
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::FILE_DELETE,
                                serde_json::to_value(FileDeleteReq {
                                    node_id: node_id.clone(),
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FileDelete { node_id });
                    }
                    OutgoingReq::FileParse { path, workspace_id } => {
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
                    }
                    OutgoingReq::PreviewGet { node_id, workspace_id, page, fit_w, fit_h } => {
                        tracing::debug!(%node_id, ?workspace_id, ?page, id, "→ preview.get");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PREVIEW_GET,
                                serde_json::to_value(PreviewGetReq {
                                    node_id: node_id.clone(),
                                    workspace_id: workspace_id.clone(),
                                    page,
                                    fit_w,
                                    fit_h,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(
                            id,
                            PendingKind::PreviewGet {
                                node_id,
                                workspace_id,
                            },
                        );
                    }
                    OutgoingReq::PreviewSetScale {
                        node_id,
                        nm_per_px,
                        workspace_id,
                    } => {
                        tracing::debug!(%node_id, nm_per_px, ?workspace_id, id,
                            "→ preview.set_scale");
                        // Isotropic from a single typed value. `nm_per_px` is the
                        // RAW/original pixel size the user entered, sent verbatim:
                        // the backend writes it to the sidecar as-is and returns
                        // the served-rescaled value for rendering, so a round-trip
                        // can never compound the downsample ratio (ADR 0034 §5).
                        let payload = serde_json::json!({
                            "node_id": node_id,
                            "workspace_id": workspace_id,
                            "physical_scale": {
                                "axes": [
                                    { "name": "x", "nm_per_px": nm_per_px },
                                    { "name": "y", "nm_per_px": nm_per_px },
                                ],
                                "unit": "nm",
                            },
                        });
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(id, op::PREVIEW_SET_SCALE, payload),
                            None,
                        )
                        .await?;
                        pending.insert(
                            id,
                            PendingKind::SetScale {
                                node_id,
                                workspace_id,
                            },
                        );
                    }
                    OutgoingReq::FigureGet { url, node_id, workspace_id } => {
                        tracing::debug!(%url, %node_id, ?workspace_id, id, "→ figure.get (preview.get)");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PREVIEW_GET,
                                serde_json::to_value(PreviewGetReq {
                                    node_id,
                                    workspace_id,
                                    page: None,
                                    fit_w: None,
                                    fit_h: None,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FigureGet { url });
                    }
                    OutgoingReq::FunctionMethods { module, name, workspace_id } => {
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
                    }
                    OutgoingReq::ReplEval { eval_id, code, mode, workspace_id } => {
                        tracing::debug!(eval_id, code_len = code.len(), ?mode, ?workspace_id, id, "→ repl.eval");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::REPL_EVAL,
                                serde_json::to_value(ReplEvalReq {
                                    eval_id,
                                    code,
                                    mode,
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::ReplEval { eval_id });
                    }
                    OutgoingReq::ReplInterrupt { workspace_id } => {
                        tracing::info!(?workspace_id, id, "→ repl.interrupt");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::REPL_INTERRUPT,
                                serde_json::json!({ "workspace_id": workspace_id }),
                            ),
                            None,
                        )
                        .await?;
                        // Fire-and-forget: the interrupt's effect arrives as
                        // streamed error+done frames, so there's no response to
                        // track in `pending`.
                    }
                    OutgoingReq::PtyOpen { cols, rows, target, user_switch } => {
                        tracing::debug!(cols, rows, ?target, user_switch, id, "→ pty.open");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PTY_OPEN,
                                serde_json::to_value(PtyOpenReq { cols, rows, target, user_switch })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::PtyOpen);
                    }
                    OutgoingReq::PtyResize { cols, rows } => {
                        // Fire-and-forget — no response, so no pending entry.
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PTY_RESIZE,
                                serde_json::to_value(PtyResizeReq { cols, rows })?,
                            ),
                            None,
                        )
                        .await?;
                    }
                    OutgoingReq::PtyWrite { bytes } => {
                        // Latency instrumentation: log every short
                        // outgoing keystroke (≤16 bytes) with a wall-
                        // clock millis stamp so the round-trip delta to
                        // the matching `pty.evt received` line is
                        // directly readable in the log.
                        if bytes.len() <= 16 {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis())
                                .unwrap_or(0);
                            tracing::info!(now_ms, n = bytes.len(), "pty.write sent");
                        }
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PTY_WRITE,
                                serde_json::to_value(PtyWriteReq { data_b64: b64 })?,
                            ),
                            None,
                        )
                        .await?;
                    }
                    OutgoingReq::PtyScroll { up } => {
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PTY_SCROLL,
                                serde_json::to_value(PtyScrollReq {
                                    direction: if up { "up" } else { "down" }.to_string(),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                    }
                    OutgoingReq::TmuxListSessions => {
                        tracing::debug!(id, "→ tmux.list_sessions");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(id, op::TMUX_LIST_SESSIONS, serde_json::json!({})),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TmuxListSessions);
                    }
                    OutgoingReq::TmuxListPanes { session } => {
                        tracing::debug!(?session, id, "→ tmux.list_panes");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TMUX_LIST_PANES,
                                serde_json::to_value(TmuxListPanesReq {
                                    session: session.clone(),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TmuxListPanes { session });
                    }
                    OutgoingReq::TmuxCreateSession { name, command, cwd } => {
                        tracing::info!(%name, ?command, ?cwd, id, "→ tmux.create_session");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TMUX_CREATE_SESSION,
                                serde_json::to_value(TmuxCreateSessionReq { name, command, cwd })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TmuxCreateSession);
                    }
                    OutgoingReq::TmuxKillSession { name } => {
                        tracing::info!(%name, id, "→ tmux.kill_session");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TMUX_KILL_SESSION,
                                serde_json::to_value(TmuxKillSessionReq { name })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TmuxKillSession);
                    }
                    OutgoingReq::TmuxCapturePane { target, lines } => {
                        tracing::debug!(%target, lines, id, "→ tmux.capture_pane");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::TMUX_CAPTURE_PANE,
                                serde_json::to_value(TmuxCapturePaneReq {
                                    target: target.clone(),
                                    lines,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::TmuxCapturePane { target });
                    }
                    OutgoingReq::DirectoryList { path } => {
                        tracing::debug!(%path, id, "→ directory.list");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::DIRECTORY_LIST,
                                serde_json::to_value(sot_protocol::DirectoryListReq {
                                    path,
                                    include_hidden: false,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::DirectoryList);
                    }
                    OutgoingReq::WorkspaceCreate { label, project_root, autostart_claude, agent } => {
                        tracing::info!(%label, %project_root, autostart_claude, %agent, id, "→ workspace.create");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::WORKSPACE_CREATE,
                                serde_json::to_value(sot_protocol::WorkspaceCreateReq {
                                    label,
                                    project_root,
                                    // Enter → ccb (comm-aware launcher, its first
                                    // turn is its own /sot-session-start); Shift+
                                    // Enter → false = bare session, no LLM agent.
                                    // agent/task stay empty either way — those are
                                    // spawned-agent fields, not used by interactive
                                    // creates.
                                    autostart_claude,
                                    agent,
                                    agent_name: String::new(),
                                    task: String::new(),
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::WorkspaceCreate);
                    }
                    OutgoingReq::WorkspaceList => {
                        tracing::debug!(id, "→ workspace.list");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::WORKSPACE_LIST,
                                serde_json::to_value(WorkspaceListReq::default())?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::WorkspaceList);
                    }
                    OutgoingReq::WorkspaceDestroy { workspace_id } => {
                        tracing::info!(%workspace_id, id, "→ workspace.destroy");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::WORKSPACE_DESTROY,
                                serde_json::to_value(sot_protocol::WorkspaceDestroyReq {
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::WorkspaceDestroy);
                    }
                    OutgoingReq::PlutoOpen { path } => {
                        tracing::info!(%path, id, "→ pluto.open");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::PLUTO_OPEN,
                                serde_json::to_value(PlutoOpenReq { path })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::PlutoOpen);
                    }
                    OutgoingReq::VideoOpen { path } => {
                        tracing::info!(%path, id, "→ video.open");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::VIDEO_OPEN,
                                serde_json::to_value(VideoOpenReq { path })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::VideoOpen);
                    }
                    OutgoingReq::DocsOpen { path } => {
                        tracing::info!(%path, id, "→ docs.open");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::DOCS_OPEN,
                                serde_json::to_value(DocsOpenReq { path })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::DocsOpen);
                    }
                    OutgoingReq::QuartoOpen { path, execute } => {
                        tracing::info!(%path, execute, id, "→ quarto.open");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::QUARTO_OPEN,
                                serde_json::to_value(QuartoOpenReq { path, execute })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::QuartoOpen);
                    }
                    OutgoingReq::ReplRunFile { eval_id, path, fresh, workspace_id } => {
                        tracing::info!(%path, fresh, eval_id, ?workspace_id, id, "→ repl.run_file");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::REPL_RUN_FILE,
                                serde_json::to_value(ReplRunFileReq {
                                    eval_id,
                                    path: path.clone(),
                                    fresh,
                                    workspace_id,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::ReplRunFile { eval_id, path, fresh });
                    }
                    OutgoingReq::FileDownload { path, dest } => {
                        tracing::info!(%path, dest = %dest.display(), id, "→ file.download");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::FILE_DOWNLOAD,
                                serde_json::to_value(FileDownloadReq { path })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FileDownload { dest, file: None });
                    }
                    OutgoingReq::FileUpload { dir, name, offset, total, eof, bytes } => {
                        tracing::debug!(%dir, %name, offset, total, eof, len = bytes.len(), id, "→ file.upload");
                        // Upload chunk bytes ride as base64 in the JSON
                        // (`data_b64`), not a trailing blob — keeps the backend's
                        // incoming-frame path simple (same as pty.write).
                        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::FILE_UPLOAD,
                                serde_json::to_value(FileUploadReq {
                                    dir,
                                    name,
                                    offset,
                                    total,
                                    eof,
                                    data_b64,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::FileUpload);
                    }
                    OutgoingReq::MonitorSubscribe => {
                        tracing::debug!(id, "→ monitor.subscribe");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(id, op::MONITOR_SUBSCRIBE, serde_json::json!({})),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::MonitorSubscribe);
                    }
                    OutgoingReq::MonitorUnsubscribe => {
                        tracing::debug!(id, "→ monitor.unsubscribe");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
                            None,
                        )
                        .await?;
                        // Fire-and-forget: the backend acks with a bare `{}` we
                        // don't track, so there's no pending entry.
                    }
                    OutgoingReq::MonitorHistory { window_s, points, until, host } => {
                        tracing::debug!(window_s, points, ?until, ?host, id, "→ monitor.history");
                        codec::write_frame(
                            &mut tx,
                            &Frame::req(
                                id,
                                op::MONITOR_HISTORY,
                                serde_json::to_value(MonitorHistoryReq {
                                    window_s,
                                    until,
                                    points,
                                    host,
                                })?,
                            ),
                            None,
                        )
                        .await?;
                        pending.insert(id, PendingKind::MonitorHistory);
                    }
                }
            }
        }
    }
}

/// Route a frame to the right `IncomingEvt`. Replies look up `id` in the
/// pending map to decide how to deserialize; everything else falls through
/// to the catch-all `Event` evt so the GPU thread can at least see it.
fn handle_response_frame(
    frame: Frame,
    mut blob: Option<Vec<u8>>,
    pending: &mut HashMap<u64, PendingKind>,
    evt_tx: &StdSender<IncomingEvt>,
) {
    if let Some(kind) = pending.remove(&frame.id) {
        match kind {
            PendingKind::TreeChildren {
                parent_id,
                workspace_id,
            } => {
                // Backend error frames ({error, code}) are legitimate
                // responses — surface them instead of tripping the struct
                // parse below ("missing field children") and dropping.
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(%parent_id, error = %err, "tree.children answered with error");
                    let _ = evt_tx.send(IncomingEvt::TreeChildrenFailed {
                        workspace_id,
                        parent_id,
                        error: err.to_string(),
                    });
                    return;
                }
                match serde_json::from_value::<TreeChildrenRes>(frame.payload) {
                Ok(res) => {
                    let _ = evt_tx.send(IncomingEvt::TreeChildren {
                        workspace_id,
                        parent_id,
                        children: res.children,
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, %parent_id, "tree.children res parse failed");
                    let _ = evt_tx.send(IncomingEvt::TreeChildrenFailed {
                        workspace_id,
                        parent_id,
                        error: e.to_string(),
                    });
                }
                }
            }
            PendingKind::TreeRoot { workspace_id } => {
                match serde_json::from_value::<TreeRootRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::TreeRoot {
                            workspace_id,
                            root: res.node,
                            children: res.children,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tree.root res parse failed");
                    }
                }
            }
            PendingKind::ModulesList { workspace_id } => {
                // The KERNEL_REQUEST envelope returns the kernel's response
                // payload verbatim. modules.list shape after Linux's
                // 4e1c8c0 is `{modules: [{name, uuid, is_main, path}, ...]}`.
                let modules: Vec<ModuleInfo> = frame
                    .payload
                    .get("modules")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                let name = m.get("name").and_then(|n| n.as_str())?;
                                let path = m.get("path").and_then(|p| p.as_str()).map(String::from);
                                Some(ModuleInfo {
                                    name: name.to_string(),
                                    path,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if modules.is_empty() {
                    tracing::warn!(payload = %frame.payload, "modules.list returned no modules");
                }
                let _ = evt_tx.send(IncomingEvt::ModulesList {
                    workspace_id,
                    modules,
                });
            }
            PendingKind::ProjectScan { workspace_id } => {
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
                    let _ = evt_tx.send(IncomingEvt::ProjectScan {
                        workspace_id,
                        project_root,
                        package_name,
                        entry_file,
                        modules: Vec::new(),
                    });
                } else {
                    let modules = payload
                        .get("modules")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().map(parse_scan_module).collect())
                        .unwrap_or_default();
                    let _ = evt_tx.send(IncomingEvt::ProjectScan {
                        workspace_id,
                        project_root,
                        package_name,
                        entry_file,
                        modules,
                    });
                }
            }
            PendingKind::MarkdownTokenize { lang, source_hash } => {
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
                let _ = evt_tx.send(IncomingEvt::MarkdownTokens {
                    lang,
                    source_hash,
                    spans,
                });
            }
            PendingKind::ConceptRead { target } => {
                match serde_json::from_value::<ConceptReadRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::ConceptRead {
                            target: res.target,
                            exists: res.exists,
                            content: res.content,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %target, "concept.read res parse failed");
                    }
                }
            }
            PendingKind::MathRender { latex, display } => {
                let is_display = display;
                match serde_json::from_value::<MathRenderRes>(frame.payload) {
                    Ok(res) => {
                        // SVG bytes ride as the framing blob.
                        // `MathRenderRes::blob` carries the descriptor
                        // (len/type) for documentation; the actual bytes
                        // are the `blob` argument from `read_frame`. Skip
                        // when the blob is missing — backend bug or a
                        // weird transport edge — log and move on.
                        let _ = res.blob;
                        match blob {
                            Some(svg_bytes) => {
                                let _ = evt_tx.send(IncomingEvt::MathRendered {
                                    latex,
                                    svg_bytes,
                                    ex: res.ex,
                                    display: res.display,
                                });
                            }
                            None => {
                                tracing::warn!(
                                    latex_len = latex.len(),
                                    is_display,
                                    "math.render reply missing blob bytes"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, latex_len = latex.len(), is_display,
                            "math.render res parse failed");
                    }
                }
            }
            PendingKind::ImageCrop { node_id } => {
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    let _ = evt_tx.send(IncomingEvt::ImageCropFailed {
                        node_id,
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<ImageCropRes>(frame.payload) {
                        Ok(res) => {
                            let _ = evt_tx.send(IncomingEvt::ImageCropped {
                                node_id,
                                path: res.path,
                                x: res.x,
                                y: res.y,
                                w: res.w,
                                h: res.h,
                                src_w: res.src_w,
                                src_h: res.src_h,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "image.crop res parse failed");
                        }
                    }
                }
            }
            PendingKind::ConceptWrite { target } => {
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
                let _ = evt_tx.send(IncomingEvt::ConceptWriteDone { target, result });
            }
            PendingKind::FileRead { node_id } => {
                match serde_json::from_value::<FileReadRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::FileRead {
                            node_id: res.node_id,
                            exists: res.exists,
                            content: res.content,
                            version: res.version,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %node_id, "file.read res parse failed");
                    }
                }
            }
            PendingKind::FileWrite { node_id } => {
                // Mirror the backend's three shapes: happy-path FileWriteRes, a
                // `{code: "conflict", current_content, current_version}`
                // envelope, or any other `{error, code}` failure.
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    if code == "conflict" {
                        let current_content = frame
                            .payload
                            .get("current_content")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let current_version = frame
                            .payload
                            .get("current_version")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        FileWriteResult::Conflict {
                            current_content,
                            current_version,
                        }
                    } else {
                        let message = frame
                            .payload
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no message)")
                            .to_string();
                        FileWriteResult::Error {
                            code: code.to_string(),
                            message,
                        }
                    }
                } else {
                    match serde_json::from_value::<FileWriteRes>(frame.payload) {
                        Ok(res) => FileWriteResult::Ok {
                            path: res.path,
                            version: res.version,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, %node_id, "file.write res parse failed");
                            FileWriteResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                let _ = evt_tx.send(IncomingEvt::FileWriteDone { node_id, result });
            }
            PendingKind::FileDelete { node_id } => {
                // Mirror the backend's two shapes: happy-path FileDeleteRes or
                // any `{error, code}` failure (`is_directory`, `not_found`, …).
                let result = if let Some(code) = frame.payload.get("code").and_then(|v| v.as_str())
                {
                    let message = frame
                        .payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)")
                        .to_string();
                    FileDeleteResult::Error {
                        code: code.to_string(),
                        message,
                    }
                } else {
                    match serde_json::from_value::<FileDeleteRes>(frame.payload) {
                        Ok(res) => FileDeleteResult::Ok {
                            path: res.path,
                            trashed: res.trashed,
                            trash_path: res.trash_path,
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, %node_id, "file.delete res parse failed");
                            FileDeleteResult::Error {
                                code: "parse_failed".to_string(),
                                message: e.to_string(),
                            }
                        }
                    }
                };
                let _ = evt_tx.send(IncomingEvt::FileDeleteDone { node_id, result });
            }
            PendingKind::FileParse { path, workspace_id } => {
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
                    let _ = evt_tx.send(IncomingEvt::FileParseFailed { workspace_id, path });
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
                let _ = evt_tx.send(IncomingEvt::FileParsed {
                    workspace_id,
                    path,
                    ast_hash,
                    definitions,
                });
            }
            PendingKind::PreviewGet {
                node_id,
                workspace_id,
            } => {
                // Same shape as the connect-time preview.get: a typed
                // PreviewGetRes envelope plus a length-prefixed blob the
                // codec already pulled out. Emit the existing
                // `IncomingEvt::Preview` so the chrome handler reuses
                // the same routing it does at startup, but include the
                // node id + workspace so figure-url resolution can
                // anchor against the right markdown directory + go to
                // the right workspace.
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::Preview {
                            node_id: Some(node_id),
                            workspace_id,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                            extras: res.extras,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "preview.get res parse failed");
                    }
                }
                return;
            }
            PendingKind::SetScale {
                node_id,
                workspace_id,
            } => {
                // ADR 0034 §5: the backend persisted the sidecar and returned
                // the RE-RENDERED preview in the same PreviewGetRes envelope,
                // with `extras.physical_scale` already rescaled for the served
                // image. Decode with the existing type and emit the existing
                // `IncomingEvt::Preview` so the chrome installs it through the
                // ONE preview path it already has — no second install path to
                // drift out of sync (the failure mode behind F2/R4-R6).
                //
                // This also has to be a REPLY, not an unsolicited push: replies
                // are correlated by frame id via `pending.remove`, so a pushed
                // preview frame would find no entry and be dropped silently.
                //
                // Rejections come back as an `error` payload under the same
                // frame id; surface them so the prompt's "saving…" resolves
                // instead of hanging.
                if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
                    let code = frame
                        .payload
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error");
                    tracing::warn!(%node_id, %code, %err, "preview.set_scale rejected");
                    let _ = evt_tx.send(IncomingEvt::ScaleSetFailed {
                        node_id,
                        message: format!("{code}: {err}"),
                    });
                    return;
                }
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::Preview {
                            node_id: Some(node_id),
                            workspace_id,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                            extras: res.extras,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "preview.set_scale res parse failed");
                    }
                }
                return;
            }
            PendingKind::FigureGet { url } => {
                // Same wire shape as PreviewGet, routed to the chrome's
                // figure cache via a different IncomingEvt so the
                // active markdown buffer isn't replaced.
                match serde_json::from_value::<PreviewGetRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::FigureLoaded {
                            url,
                            mime: res.mime,
                            bytes: blob.unwrap_or_default(),
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, %url, "figure.get res parse failed");
                    }
                }
                return;
            }
            PendingKind::FunctionMethods { module, name, workspace_id } => {
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
                let _ = evt_tx.send(IncomingEvt::FunctionMethodsReceived {
                    workspace_id,
                    module,
                    name,
                    methods,
                });
            }
            PendingKind::ReplEval { eval_id } => {
                // Synchronous-collect per ADR 0009: the response carries
                // the full frame list. Streamed delivery is a planned
                // enhancement and shifts the routing
                // off this path; this arm only needs to handle the
                // collected-at-once payload.
                match serde_json::from_value::<ReplEvalRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::ReplEvalDone {
                            eval_id: res.eval_id,
                            elapsed_ms: res.elapsed_ms,
                            frames: res.frames,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, eval_id, "repl.eval res parse failed");
                    }
                }
            }
            PendingKind::PtyOpen => match serde_json::from_value::<PtyOpenRes>(frame.payload) {
                Ok(res) => {
                    let _ = evt_tx.send(IncomingEvt::PtyOpened {
                        cols: res.cols,
                        rows: res.rows,
                        pane_command: res.pane_command,
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "pty.open res parse failed");
                }
            },
            PendingKind::TmuxListSessions => {
                match serde_json::from_value::<TmuxListSessionsRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::TmuxSessions {
                            sessions: res.sessions,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tmux.list_sessions res parse failed");
                    }
                }
            }
            PendingKind::TmuxListPanes { session } => {
                match serde_json::from_value::<TmuxListPanesRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::TmuxPanes {
                            session,
                            panes: res.panes,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tmux.list_panes res parse failed");
                    }
                }
            }
            PendingKind::TmuxCreateSession => {
                let result = tmux_op_result(&frame.payload);
                let _ = evt_tx.send(IncomingEvt::TmuxSessionCreated { result });
            }
            PendingKind::TmuxKillSession => {
                let result = tmux_op_result(&frame.payload);
                let _ = evt_tx.send(IncomingEvt::TmuxSessionKilled { result });
            }
            PendingKind::TmuxCapturePane { target } => {
                match serde_json::from_value::<TmuxCapturePaneRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::TmuxPaneCaptured {
                            target,
                            text: res.text,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tmux.capture_pane res parse failed");
                    }
                }
            }
            PendingKind::DirectoryList => {
                match serde_json::from_value::<sot_protocol::DirectoryListRes>(frame.payload) {
                    Ok(res) => {
                        let entries: Vec<DirEntry> = res
                            .entries
                            .into_iter()
                            .map(|e| DirEntry {
                                name: e.name,
                                path: e.path,
                                has_children: e.has_children,
                            })
                            .collect();
                        let _ = evt_tx.send(IncomingEvt::DirectoryList {
                            path: res.path,
                            entries,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "directory.list res parse failed");
                    }
                }
            }
            PendingKind::WorkspaceCreate => {
                // Backend returns either WorkspaceCreateRes on success
                // or `{error, code}` on failure (no_such_path etc).
                // Distinguish by presence of `workspace_id`.
                let payload = frame.payload;
                let result = if payload.get("workspace_id").is_some() {
                    match serde_json::from_value::<sot_protocol::WorkspaceCreateRes>(payload) {
                        Ok(r) => Ok(WorkspaceCreatedInfo {
                            workspace_id: r.workspace_id,
                            slug: r.slug,
                            label: r.label,
                            project_root: r.project_root,
                            tmux_session: r.tmux_session,
                        }),
                        Err(e) => Err(format!("workspace.create res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::WorkspaceCreated { result });
            }
            PendingKind::WorkspaceList => {
                match serde_json::from_value::<WorkspaceListRes>(frame.payload) {
                    Ok(res) => {
                        let workspaces: Vec<WorkspaceInfo> = res
                            .workspaces
                            .into_iter()
                            .map(|w| WorkspaceInfo {
                                workspace_id: w.workspace_id,
                                slug: w.slug,
                                label: w.label,
                                project_root: w.project_root,
                                tmux_session: w.tmux_session,
                                kernel_running: w.kernel_running,
                                is_default: w.is_default,
                                autostart_claude: w.autostart_claude,
                                agent_name: w.agent_name,
                                task: w.task,
                                agent_state: w.agent_state,
                                agent_summary: w.agent_summary,
                                agent_status_at: w.agent_status_at,
                            })
                            .collect();
                        let _ = evt_tx.send(IncomingEvt::Workspaces { workspaces });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "workspace.list res parse failed");
                    }
                }
            }
            PendingKind::WorkspaceDestroy => {
                // Same shape as WorkspaceCreate: success carries the
                // canonical fields (workspace_id etc.), failure carries
                // `{error, code}`. Distinguish by presence of
                // `workspace_id` since the protocol re-uses the op
                // response frame for both.
                let payload = frame.payload;
                let result = if payload.get("workspace_id").is_some() {
                    match serde_json::from_value::<sot_protocol::WorkspaceDestroyRes>(payload) {
                        Ok(r) => Ok(WorkspaceDestroyedInfo {
                            workspace_id: r.workspace_id,
                            slug: r.slug,
                            label: r.label,
                            tmux_killed: r.tmux_killed,
                            toml_removed: r.toml_removed,
                        }),
                        Err(e) => Err(format!("workspace.destroy res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::WorkspaceDestroyed { result });
            }
            PendingKind::PlutoOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<PlutoOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("pluto.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::PlutoOpened { result });
            }
            PendingKind::VideoOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<VideoOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("video.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::VideoOpened { result });
            }
            PendingKind::DocsOpen => {
                let payload = frame.payload;
                let result = if payload.get("url").is_some() {
                    match serde_json::from_value::<DocsOpenRes>(payload) {
                        Ok(r) => Ok(r.url),
                        Err(e) => Err(format!("docs.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::DocsOpened { result });
            }
            PendingKind::QuartoOpen => {
                let payload = frame.payload;
                let result = if payload.get("html_base64").is_some() {
                    match serde_json::from_value::<QuartoOpenRes>(payload) {
                        Ok(r) => base64::engine::general_purpose::STANDARD
                            .decode(r.html_base64.as_bytes())
                            .map_err(|e| format!("quarto.open base64 decode: {e}")),
                        Err(e) => Err(format!("quarto.open res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = evt_tx.send(IncomingEvt::QuartoOpened { result });
            }
            PendingKind::ReplRunFile {
                eval_id,
                path,
                fresh,
            } => {
                // Success = `frames` present; error envelopes carry
                // `{error, code}` per the handler contract. Same shape
                // pattern as WorkspaceCreate / PlutoOpen above.
                let payload = frame.payload;
                let result = if payload.get("frames").is_some() {
                    match serde_json::from_value::<ReplRunFileRes>(payload) {
                        Ok(r) => Ok(ReplRunFileInfo {
                            eval_id: r.eval_id,
                            path: r.path,
                            fresh: r.fresh,
                            elapsed_ms: r.elapsed_ms,
                            project_dir: r.project_dir,
                            project_source: r.project_source,
                            frames: r.frames,
                        }),
                        Err(e) => Err(format!("repl.run_file res parse: {e}")),
                    }
                } else {
                    let msg = payload
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(msg)
                };
                let _ = path;
                let _ = fresh;
                let _ = evt_tx.send(IncomingEvt::ReplRunFileDone { eval_id, result });
            }
            PendingKind::FileDownload { dest, mut file } => {
                let frame_id = frame.id;
                let payload = frame.payload;
                // A download error replies with `{error, code}` and no chunk.
                if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(error = %err, dest = %dest.display(), "file.download failed");
                    if file.is_some() {
                        let _ = std::fs::remove_file(&dest);
                    }
                    let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                        op: "download",
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<FileChunk>(payload) {
                        Ok(chunk) => {
                            let bytes = blob.take().unwrap_or_default();
                            // Create (truncating) the dest on the first chunk so a
                            // pre-chunk error never leaves an empty file behind.
                            if file.is_none() {
                                match std::fs::File::create(&dest) {
                                    Ok(f) => file = Some(f),
                                    Err(e) => {
                                        tracing::warn!(error = %e, dest = %dest.display(),
                                            "file.download: cannot create dest");
                                        let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                                            op: "download",
                                            message: format!("create {}: {e}", dest.display()),
                                        });
                                        return;
                                    }
                                }
                            }
                            let mut write_err: Option<String> = None;
                            if let Some(f) = file.as_mut() {
                                use std::io::{Seek, SeekFrom, Write};
                                if let Err(e) = f
                                    .seek(SeekFrom::Start(chunk.offset))
                                    .and_then(|_| f.write_all(&bytes))
                                {
                                    write_err = Some(format!("write {}: {e}", dest.display()));
                                }
                            }
                            if let Some(msg) = write_err {
                                tracing::warn!(dest = %dest.display(), %msg, "file.download write failed");
                                let _ = std::fs::remove_file(&dest);
                                let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                                    op: "download",
                                    message: msg,
                                });
                            } else {
                                let written = chunk.offset + bytes.len() as u64;
                                let _ = evt_tx.send(IncomingEvt::FileDownloadProgress {
                                    dest: dest.clone(),
                                    written,
                                    total: chunk.total,
                                    eof: chunk.eof,
                                });
                                if !chunk.eof {
                                    // One request id, many chunks: keep the
                                    // transfer (with its open file handle) alive
                                    // for the next streamed frame.
                                    pending
                                        .insert(frame_id, PendingKind::FileDownload { dest, file });
                                }
                                // On eof: pending stays removed; `file` drops here,
                                // flushing + closing the completed download.
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "file.download chunk parse failed");
                            let _ = std::fs::remove_file(&dest);
                            let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                                op: "download",
                                message: format!("bad chunk: {e}"),
                            });
                        }
                    }
                }
            }
            PendingKind::FileUpload => {
                let payload = frame.payload;
                if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(error = %err, "file.upload failed");
                    let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                        op: "upload",
                        message: err.to_string(),
                    });
                } else {
                    match serde_json::from_value::<FileUploadAck>(payload) {
                        Ok(ack) => {
                            let _ = evt_tx.send(IncomingEvt::FileUploadAck {
                                offset: ack.offset,
                                done: ack.done,
                                final_name: ack.final_name,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "file.upload ack parse failed");
                            let _ = evt_tx.send(IncomingEvt::FileTransferFailed {
                                op: "upload",
                                message: format!("bad ack: {e}"),
                            });
                        }
                    }
                }
            }
            PendingKind::MonitorSubscribe => {
                match serde_json::from_value::<MonitorSubscribeRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::MonitorSubscribed {
                            hosts: res.hosts,
                            interval_s: res.interval_s,
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "monitor.subscribe res parse failed"),
                }
            }
            PendingKind::MonitorHistory => {
                match serde_json::from_value::<MonitorHistoryRes>(frame.payload) {
                    Ok(res) => {
                        let _ = evt_tx.send(IncomingEvt::MonitorHistory { hosts: res.hosts });
                    }
                    Err(e) => tracing::warn!(error = %e, "monitor.history res parse failed"),
                }
            }
        }
        let _ = blob; // remaining ops carry no blob (download took its own)
        return;
    }
    // Unsolicited evt frames. The backend uses one for `pty.evt` —
    // pty byte streams piped through to the chrome's terminal
    // emulator. Decode base64 here so the consumer sees raw bytes.
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::PTY_EVT {
        let data_b64 = frame
            .payload
            .get("data_b64")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes()) {
            Ok(bytes) => {
                // Latency instrumentation: log every pty.evt arrival
                // regardless of payload size — even a single-keystroke
                // echo can come back as a multi-byte ANSI sequence.
                // Lets us see the keystroke→echo delta directly in the
                // log against the matching `pty.write sent` line.
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                tracing::info!(now_ms, n = bytes.len(), "pty.evt received");
                let _ = evt_tx.send(IncomingEvt::PtyBytes { bytes });
            }
            Err(e) => tracing::warn!(error = %e, "pty.evt b64 decode failed"),
        }
        return;
    }
    // Streamed REPL output frame (`repl.frame` evt, ADR 0009 phase-2). One
    // frame, pushed as produced; the consumer appends it to the in-flight
    // scrollback entry live (`Done` is terminal).
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::REPL_FRAME {
        match serde_json::from_value::<ReplFrameEvt>(frame.payload) {
            Ok(ev) => {
                let _ = evt_tx.send(IncomingEvt::ReplFrameStreamed {
                    eval_id: ev.eval_id,
                    workspace_id: ev.workspace_id,
                    frame: ev.frame,
                });
            }
            Err(e) => tracing::warn!(error = %e, "repl.frame evt parse failed"),
        }
        return;
    }
    // Live server-metrics tick (`monitor.tick` evt, ADR 0020). One sample per
    // host at the subscribed cadence; the consumer appends each to its ring.
    if frame.kind == sot_protocol::Kind::Evt && frame.op == op::MONITOR_TICK {
        match serde_json::from_value::<MonitorTickEvt>(frame.payload) {
            Ok(ev) => {
                let _ = evt_tx.send(IncomingEvt::MonitorTick { hosts: ev.hosts });
            }
            Err(e) => tracing::warn!(error = %e, "monitor.tick evt parse failed"),
        }
        return;
    }
    let _ = evt_tx.send(IncomingEvt::Event {
        op: frame.op,
        payload: frame.payload,
    });
    if blob.is_some() {
        // Spike handlers don't emit unsolicited blobs; drop and move on.
    }
}

fn take_id(next_id: &mut u64) -> u64 {
    let id = *next_id;
    *next_id += 1;
    id
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

/// Translate the backend's tmux op reply payload into a Result. Backend
/// returns `{name: "..."}` on success and `{error, code: "tmux_failed"}`
/// on failure (see `handlers.rs::tmux_error_frame`). The chrome wants a
/// single Result-shaped event for both shapes.
fn tmux_op_result(payload: &Value) -> Result<String, String> {
    if let Some(code) = payload.get("code").and_then(|v| v.as_str()) {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(code)
            .to_string();
        return Err(msg);
    }
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(name)
}
