// ops.rs — typed payloads for the M1-spike ops.
//
// Each `*Req` / `*Res` struct serializes into the `payload` field of a Frame.
// The codec doesn't know about these types; senders construct a Frame whose
// payload is `serde_json::to_value(req)?`, receivers route on `frame.op` and
// then `serde_json::from_value(payload)`.

use serde::{Deserialize, Serialize};

use crate::ir::{BlobDescriptor, TreeNode};

/// Op verbs as constants so frontend/backend can match on `frame.op` against
/// the same source-of-truth strings.
pub mod op {
    pub const HELLO: &str = "hello";
    pub const TREE_ROOT: &str = "tree.root";
    pub const TREE_CHILDREN: &str = "tree.children";
    /// Flip the per-workspace "show hidden files" flag for Files mode and
    /// invalidate the files tree (bumps `tree.invalidate` on the session ring
    /// so a reconnecting client re-fetches; the live frontend re-fetches
    /// `tree.root` immediately after). Request is `ToggleHiddenReq`, response
    /// `ToggleHiddenRes { show_hidden }` carrying the NEW state.
    pub const NAV_TOGGLE_HIDDEN: &str = "nav.toggle_hidden";
    pub const PREVIEW_GET: &str = "preview.get";
    /// Crop a rectangular region (in source-image pixel coords) out of an
    /// image node and write it as a PNG under `<workspace>/.sot/captures/`
    /// on the backend, returning the path. Used by the "send the zoomed ROI
    /// to the LLM pane" feature (ADR 0022): the FE computes the visible ROI
    /// from its zoom/pan, the backend crops from the *source* file (full
    /// fidelity, not a screen grab), and the in-pane `claude` (also on the
    /// backend) `Read`s the returned path.
    pub const IMAGE_CROP: &str = "image.crop";
    pub const MATH_RENDER: &str = "math.render";
    /// Generic Julia-kernel proxy. Payload `{kernel_op, kernel_payload}`;
    /// response payload is the kernel's response payload verbatim. Lets the
    /// frontend exercise kernel features (`modules.list`, `file.parse`, …)
    /// without adding a wire op per kernel verb.
    pub const KERNEL_REQUEST: &str = "kernel.request";
    pub const CONCEPT_READ: &str = "concept.read";
    pub const CONCEPT_WRITE: &str = "concept.write";
    pub const CONCEPT_LIST: &str = "concept.list";
    pub const FILE_READ: &str = "file.read";
    pub const FILE_WRITE: &str = "file.write";
    /// Trash a file from Files-mode nav. v1 refuses directories and never
    /// hard-unlinks: system trash (`gio trash`) when available, else a move
    /// to `<workspace_root>/.sot-trash/` — recoverable either way.
    pub const FILE_DELETE: &str = "file.delete";
    pub const REPL_EVAL: &str = "repl.eval";
    /// Run a `.jl` file either in the persistent REPL (`fresh:false`,
    /// via `include`) or in a fresh `julia` subprocess (`fresh:true`).
    /// Project is auto-detected by walking up from the file path. Same
    /// frame stream shape as `repl.eval`.
    pub const REPL_RUN_FILE: &str = "repl.run_file";
    pub const REPL_INTERRUPT: &str = "repl.interrupt";
    /// Server→client push: one REPL output frame, streamed as it is produced
    /// (ADR 0009 phase-2). Replaces the synchronous-collect model where every
    /// frame rode the `repl.eval` / `repl.run_file` *response*. The kernel now
    /// runs the eval in a task and emits each frame immediately; the backend
    /// forwards it as a `repl.frame` evt. Payload is `ReplFrameEvt`
    /// (`{eval_id, workspace_id?, frame}`). The `done` frame is terminal — no
    /// further frames for that `eval_id` follow. The `repl.eval` /
    /// `repl.run_file` response is now a terminal ack (empty `frames`).
    pub const REPL_FRAME: &str = "repl.frame";
    /// LLM-pane terminal — attach (or create) the shared `sot-llm`
    /// tmux session on a pty. The backend keeps a single pty per
    /// connection and streams its bytes back via `PTY_EVT` event
    /// frames; the frontend sends keystrokes back as `PTY_WRITE`
    /// requests (fire-and-forget, no response).
    pub const PTY_OPEN: &str = "pty.open";
    pub const PTY_RESIZE: &str = "pty.resize";
    pub const PTY_WRITE: &str = "pty.write";
    pub const PTY_EVT: &str = "pty.evt";
    /// Server-pushed evt: a file under the project root changed on disk.
    /// Carries `{path, node_id?, kind}` (kind ∈ "modified" | "created" |
    /// "removed"). Frontend re-fetches preview if the path matches a
    /// cursored / pinned node; otherwise ignores. Emitted by the
    /// notify-based watcher and bumped on the session ring so a reconnecting
    /// client catches changes that happened while it was away.
    pub const PREVIEW_CHANGED: &str = "preview.changed";
    /// Backend-sessions registry per ADR 0013. Shells out to the host
    /// tmux server. Sessions mode (frontend) consumes these to enumerate
    /// live backends, spawn new ones, kill, and live-tail panes.
    pub const TMUX_LIST_SESSIONS: &str = "tmux.list_sessions";
    pub const TMUX_LIST_PANES: &str = "tmux.list_panes";
    pub const TMUX_CREATE_SESSION: &str = "tmux.create_session";
    pub const TMUX_KILL_SESSION: &str = "tmux.kill_session";
    pub const TMUX_CAPTURE_PANE: &str = "tmux.capture_pane";
    /// List the immediate subdirectories of `path`. Used by the Sessions-
    /// mode workspace picker so the user can browse the filesystem
    /// instead of typing a path. Returns one entry per directory: name
    /// (basename), full path, and a `has_children` flag for tree
    /// rendering. Hidden directories (leading dot) are excluded unless
    /// `include_hidden` is set; symlinks are followed for the entry but
    /// not for recursion.
    pub const DIRECTORY_LIST: &str = "directory.list";
    /// Register a new workspace with the daemon (ADR 0014). Creates a
    /// per-workspace toml, registers in-memory, and creates a tmux
    /// session at `sot-be-<slug>` so BL pane attach works. Does
    /// *not* spawn a second daemon — kernel + repl live inside the
    /// existing daemon, gated by the workspace_id this op returns.
    pub const WORKSPACE_CREATE: &str = "workspace.create";
    /// Enumerate the daemon's registered workspaces (ADR 0014). Empty
    /// request payload; response carries one entry per workspace with
    /// the metadata Sessions mode needs (slug, label, project_root,
    /// kernel-handle-constructed flag). The default workspace is
    /// included in the list and marked via `is_default`.
    pub const WORKSPACE_LIST: &str = "workspace.list";
    /// Destroy a registered workspace (ADR 0014). Backend shuts down
    /// the workspace's kernel child if it was spawned, kills the
    /// `sot-be-<slug>` tmux session, removes the toml from disk,
    /// and drops the in-memory registry entry. Idempotent: destroying
    /// an unknown id returns ok with no side effects. The default
    /// workspace is *not* destroyable — request returns an error.
    pub const WORKSPACE_DESTROY: &str = "workspace.destroy";
    /// Server→client push fired when a workspace is created or destroyed
    /// (ADR 0014). Mirrors `preview.changed`: the daemon broadcasts to every
    /// connection so the Sessions strip refreshes live instead of waiting for
    /// a manual `workspace.list` poll. Payload carries `action`
    /// ("created" | "destroyed"), `slug`, and `workspace_id`; the frontend
    /// reacts by re-issuing `workspace.list`.
    pub const WORKSPACE_CHANGED: &str = "workspace.changed";
    /// Client→daemon request: relay an agent-to-agent message through the
    /// backend so it reaches every connected frontend instantly over the
    /// SSH-forwarded socket (the only live cross-machine link). The daemon
    /// stamps a `ts` and publishes onto a broadcast channel; each connection
    /// turns it into an `AGENT_MESSAGE` evt. Payload is `AgentSendReq`
    /// (`{from, to, text}`; `to == ""` means broadcast/all). Response is a
    /// simple `AgentSendRes{ok}` ack. Structurally mirrors `WORKSPACE_CHANGED`
    /// but adds the client→daemon publish leg.
    pub const AGENT_SEND: &str = "agent.send";
    /// Server→client push fired for every relayed agent message (see
    /// `AGENT_SEND`). Mirrors `workspace.changed`: the daemon broadcasts to
    /// every connection so the in-terminal agent on the other machine receives
    /// the message instantly instead of polling the slow git bus. Payload
    /// carries `{from, to, text, ts}`; the frontend appends it as one JSON line
    /// to `<state-dir>/fe-inbox.jsonl`.
    pub const AGENT_MESSAGE: &str = "agent.message";
    /// Client→daemon request: drive the frontend(s) with an imperative UI command
    /// (ADR 0025). Mirrors `AGENT_SEND`'s publish leg — the daemon re-emits the
    /// body as an `FE_COMMAND` evt to connected FEs. Unlike the relay `nav.preview`
    /// envelope (gated, workspace-scoped), this is *imperative*: the FE switches +
    /// shows regardless of its current view, under a badge-floor + opt-in
    /// force-show consent model (FE-side). Payload `FeCommandSendReq`
    /// (`{cmd, args, target?}`); response `FeCommandSendRes{ok}`. Sent by the
    /// `sot-fe` BE CLI over the daemon socket — no comm relay, no FE LLM.
    pub const FE_COMMAND_SEND: &str = "fe.command.send";
    /// Server→client push carrying one imperative FE command (ADR 0025). Mirrors
    /// `AGENT_MESSAGE`: the daemon broadcasts to every connection; the FE parses
    /// the `{v, cmd, args}` envelope into an `FeCommand` and runs it through the
    /// existing `dispatch_fe_command` sink. `target` (a FE sot-comm handle)
    /// optionally scopes it: `None` → every FE acts (the badge floor);
    /// `Some(handle)` → only the matching FE acts (force-show to a specific FE;
    /// v1.1 will route via daemon primary-tracking instead). Payload `FeCommandEvt`.
    pub const FE_COMMAND: &str = "fe.command";
    /// Open a `.jl` Pluto-flavored notebook in the backend-supervised
    /// Pluto server. The backend lazy-spawns one shared server per
    /// daemon (listening on 127.0.0.1:1234), keeps it across calls,
    /// and returns the per-notebook `/edit?id=<uuid>` URL the frontend
    /// hands to the OS browser-open.
    pub const PLUTO_OPEN: &str = "pluto.open";
    /// Open a video file in the OS browser (HTML5 <video>, native decode).
    /// The backend serves the file over a loopback HTTP server with byte-range
    /// support and returns the URL; the launcher SSH-forwards the port. ADR 0018.
    pub const VIDEO_OPEN: &str = "video.open";
    /// Open the project's built Documenter site (`docs/build`) in the OS
    /// browser. The backend serves the static site tree over a loopback HTTP
    /// server (rooted at the build dir) and returns the URL; the launcher
    /// SSH-forwards the port. Full CSS/JS/sub-page fidelity, unlike `o`'s
    /// single-file file:// open. ADR 0024.
    pub const DOCS_OPEN: &str = "docs.open";
    /// Render a Quarto/markdown doc to a self-contained HTML and return the
    /// bytes for the frontend to open in the OS browser (via a local temp
    /// file — same path as a `text/html` preview's `o`). `execute` selects the
    /// fast structure-only render (`o`) vs. running code chunks (`O`).
    pub const QUARTO_OPEN: &str = "quarto.open";
    /// Stream a backend-host file down to the frontend machine in <=1 MiB
    /// chunks over the existing authenticated socket (no scp/HTTP/platform
    /// tools). Reads any path the backend can read (same reach as preview.get,
    /// so files outside the project root work). Response = a sequence of frames
    /// sharing the request id, each a `FileChunk` JSON + the chunk bytes as the
    /// trailing blob; the `eof = true` frame carries the final chunk.
    pub const FILE_DOWNLOAD: &str = "file.download";
    /// Upload a frontend file to the backend host in <=1 MiB chunks. Each chunk
    /// is a `file.upload` req (`FileUploadReq`, chunk bytes base64 in
    /// `data_b64`); the backend writes into the cursored directory (`dir`)
    /// under a sanitized basename (`name`), truncating on `offset == 0`, and
    /// acks each with `FileUploadAck`.
    pub const FILE_UPLOAD: &str = "file.upload";
    /// Server monitoring (ADR 0020). Start this connection's live metrics
    /// stream at `interval_s` cadence; the backend's reactive sampler polls the
    /// Netdata parent and pushes `monitor.tick` evts until `monitor.unsubscribe`
    /// (or the connection drops). The initial window fill is a separate
    /// `monitor.history` call, so subscribe stays pure lifecycle. Payload
    /// `MonitorSubscribeReq`; response `MonitorSubscribeRes`.
    pub const MONITOR_SUBSCRIBE: &str = "monitor.subscribe";
    /// Stop this connection's live metrics stream (ADR 0020). Empty payload;
    /// the backend tears the sampler down when the last subscriber leaves
    /// ("reactive over eager"). Response is a bare ack.
    pub const MONITOR_UNSUBSCRIBE: &str = "monitor.unsubscribe";
    /// Fetch a historical metrics window for one or all hosts (ADR 0020),
    /// served from the Netdata parent's tiered storage and downsampled to
    /// ~`points`. Used for the initial drawer fill and for every time-axis
    /// rescale (log/zoom) that reaches past the live ring buffer — the same
    /// path serves first paint and rescale. Payload `MonitorHistoryReq`;
    /// response `MonitorHistoryRes`.
    pub const MONITOR_HISTORY: &str = "monitor.history";
    /// Server→client push: one fresh sample per host (ADR 0020), streamed at
    /// the subscribed cadence. A host that went unreachable for the interval
    /// appears with `stale: true` and no sample so the frontend advances the
    /// axis and draws a gap, never a flatline (ADR 0020 §5). Payload
    /// `MonitorTickEvt`.
    pub const MONITOR_TICK: &str = "monitor.tick";
    /// On-demand "check for updates" (ADR 0030 §4, Phase C). Empty request
    /// (`UpdateCheckReq`); the backend queries the GitHub Releases API for the
    /// latest release, compares it against its embedded `app_version()`, and
    /// answers `UpdateCheckRes { current, latest, update_available, staged,
    /// status }`. A dev build answers `status = "disabled: dev build"` and
    /// never checks; a gh-absent / not-authed / network failure answers
    /// `status = "check unavailable: <why>"` rather than erroring. The daemon
    /// also runs this check on a daily timer and pushes an `FE_COMMAND`
    /// `notify` when a newer release appears; this op is the manual trigger.
    pub const UPDATE_CHECK: &str = "update.check";
}

/// Connect handshake. Per ADR 0010, every connect carries
/// `(session_id, client_id, last_seen_revision)`. First-time connect leaves
/// `session_id` as None and accepts whatever the backend assigns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReq {
    pub client_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub last_seen_revision: u64,
    /// App-level token. Backend resolution order: `--token` (compat) else
    /// `$SOT_TOKEN` else the canonical `${XDG_CONFIG_HOME:-~/.config}/sot/token`
    /// file (a single value, not a `tokens.toml` — that path is stale).
    /// Required whenever the backend has one configured, even on Unix-socket
    /// transport since the remote may be shared.
    #[serde(default)]
    pub token: Option<String>,
    /// Wire-contract protocol version the client speaks (ADR 0030 §2). The
    /// backend gates the handshake on integer equality against its own
    /// `PROTOCOL_VERSION`. `#[serde(default)]` → `0` for a pre-versioning peer
    /// that predates this field; `0` is treated as "pre-versioning" and gets a
    /// one-time transition grace while the backend's own version is still 1.
    #[serde(default)]
    pub protocol: u32,
    /// The client's product version string (`sot_protocol::app_version()`),
    /// e.g. `0.1.0-dev+abc`. `#[serde(default)]` → `""` for a pre-versioning
    /// peer. Reported back verbatim in a protocol-mismatch error so the user
    /// sees both sides' versions.
    #[serde(default)]
    pub app_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloRes {
    pub session_id: String,
    pub revision: u64,
    /// True if the backend will follow up with a snapshot because the client
    /// fell off the back of the event ring. False means the client is caught
    /// up enough to take it from here without help.
    pub snapshot_pending: bool,
    /// Backend-reported hostname (`gethostname`). Lets the chrome show
    /// "connected to myhost" instead of just a session id. Optional so an
    /// older backend that doesn't fill it still works — frontend falls
    /// back to the CLI transport target in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The directory the backend is serving (`--project-root`), so the
    /// chrome can show "myhost:Ship of Tools" rather than just the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Optional human-friendly label passed to the backend via `--label`
    /// (or the env var). Sessions mode uses this to match the running
    /// daemon to its `~/.config/sot/sessions/<id>.toml` entry per ADR
    /// 0013. Absent when the backend was launched the old way without a
    /// label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many frontend connections are live on this backend *including*
    /// the one this hello answers (ADR 0010/0013 multi-frontend). 1 in the
    /// normal single-FE case; >1 when the user has another device attached.
    /// `#[serde(default)]` so an older backend that omits it deserializes
    /// to 0 on a newer frontend.
    #[serde(default)]
    pub clients_connected: usize,
    /// The backend's wire-contract protocol version (ADR 0030 §2), mirror of
    /// `HelloReq::protocol`. `#[serde(default)]` → `0` for a pre-versioning
    /// backend that predates the field; the frontend `tracing::warn!`s when a
    /// successful hello comes back with `0` (legacy backend) but still runs.
    #[serde(default)]
    pub protocol: u32,
    /// The backend's product version string (`sot_protocol::app_version()`).
    /// `#[serde(default)]` → `""` for a pre-versioning backend.
    #[serde(default)]
    pub app_version: String,
}

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
    /// kernel handle so `modules.list` / `file.parse` etc. see the
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

/// Submit a chunk of Julia code to the persistent REPL. Phase-1 is
/// synchronous-collect: the response carries the full `frames` list
/// (stdout, stderr, value, error, done) for this eval. Streamed evt
/// delivery is a phase-2 enhancement; the payload shape doesn't change.
///
/// `mode` is optional ("julia" | "pkg", default "julia") — `"pkg"`
/// routes the line through `Pkg.REPLMode.do_cmds` for `pkg>`-style
/// commands (`b07b4f0`). Omitted on the wire when None so the
/// envelope stays compatible with older backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplEvalReq {
    pub eval_id: u64,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// ADR 0014 workspace routing. The backend resolves this to the
    /// per-workspace `Repl` handle so `x = 5` in workspace A doesn't
    /// leak into workspace B. Missing = default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplEvalRes {
    pub eval_id: u64,
    pub elapsed_ms: u64,
    /// Phase-2 streaming: frames now arrive as `repl.frame` evts, so this
    /// is empty on the response (terminal ack). `#[serde(default)]` keeps
    /// the field optional on the wire for forward/backward compatibility.
    #[serde(default)]
    pub frames: Vec<ReplFrame>,
}

/// Run a `.jl` file. `fresh:true` spawns a one-shot `julia --project=...`
/// subprocess (no `value` / `image` frames possible — only stdout/stderr/
/// error/done). `fresh:false` calls `include(path)` inside the persistent
/// REPL, delivering the same frame shapes as `repl.eval`. Project is
/// auto-detected by the REPL (walking up from `path` for `Project.toml`);
/// the persistent REPL's active project is the fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplRunFileReq {
    pub eval_id: u64,
    pub path: String,
    #[serde(default)]
    pub fresh: bool,
    /// ADR 0014 workspace routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplRunFileRes {
    pub eval_id: u64,
    pub path: String,
    pub fresh: bool,
    pub elapsed_ms: u64,
    /// Directory passed as `--project=` (when fresh:true) or — for
    /// fresh:false — the project the REPL believes the file belongs to,
    /// surfaced so the frontend can warn on mismatches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    /// "discovered" (a Project.toml was found), "fallback" (REPL's
    /// active project), or "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_source: Option<String>,
    /// Phase-2 streaming: see `ReplEvalRes::frames`. Empty on the response;
    /// frames stream as `repl.frame` evts.
    #[serde(default)]
    pub frames: Vec<ReplFrame>,
}

/// Per ADR 0009. `kind`-tagged enum on the wire so new frame kinds (image
/// blobs, html, …) land additively.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReplFrame {
    Stdout {
        text: String,
    },
    Stderr {
        text: String,
    },
    Value {
        mime: String,
        text: String,
    },
    /// Result is a showable binary (CairoMakie figure, raster Plot, …).
    /// The REPL prefers this over `value` when `showable(MIME"image/...",
    /// result)` is true so the frontend can rasterise without parsing
    /// `text/plain`. Bytes are inline base64 — same convention as
    /// `file.preview`'s `blob_base64` — to keep the NDJSON envelope JSON-
    /// safe without growing a sidecar-blob path through the REPL bridge.
    Image {
        mime: String,
        data_base64: String,
        bytes: u64,
    },
    Error {
        message: String,
        stacktrace: Vec<StackFrame>,
    },
    Done {
        eval_id: u64,
        elapsed_ms: u64,
    },
}

/// Payload of a `repl.frame` evt (ADR 0009 phase-2 streaming). One frame,
/// pushed as it is produced. `eval_id` correlates the frame to the originating
/// `repl.eval` / `repl.run_file` request; `workspace_id` lets a frontend that
/// has swapped workspaces (ADR 0014) route the frame to the right pane even
/// after the active workspace changed. The inner `frame` is the same tagged
/// `ReplFrame` that previously rode the response, unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplFrameEvt {
    pub eval_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub frame: ReplFrame,
}

/// Open (or attach) a tmux session on a pty. The backend spawns
/// `tmux new-session -A -s <target>` on a pty sized (cols, rows) and
/// streams bytes back via `PTY_EVT` events.
///
/// `target` selects the tmux session: `None` defaults to `sot-llm`
/// (the shared LLM pane); Sessions mode (ADR 0013) passes a backend
/// session name so the BL pane shows that backend's tmux session
/// instead. Calling `pty.open` again with the same target just resizes;
/// with a different target the backend kills the existing pty and
/// spawns a fresh one — frontend sees that as a clean transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyOpenReq {
    pub cols: u16,
    pub rows: u16,
    /// Tmux session name. `None` → `sot-llm`. Sessions mode passes the
    /// `sot-be-<slug>` name from ADR 0013 paths.
    #[serde(default)]
    pub target: Option<String>,
    /// True ONLY when this open is an explicit USER workspace-switch (the FE
    /// sets it at `switch_to_workspace` → `attach_session_to_bl`). The daemon
    /// re-targets the single foreground pty (ADR-0014) to a DIFFERENT session
    /// ONLY when this is true; a background/roaming re-attach or a daemon-boot
    /// open leaves it false so it can't yank the foreground away from where the
    /// user put it (the #5 single-pty thrash that froze create-session for
    /// ~1min). `serde(default)` = false keeps the wire compatible with an FE
    /// that predates the field.
    #[serde(default)]
    pub user_switch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyOpenRes {
    pub cols: u16,
    pub rows: u16,
    /// Foreground command of the (re-)targeted session's active pane at
    /// attach time — e.g. `claude` when an agent session already has claude
    /// running, `bash` at a shell prompt. The frontend uses this as the
    /// authoritative "is claude already up here?" signal to suppress a
    /// redundant autostart launch (which would otherwise land in the live
    /// agent's prompt). Populated on a fresh spawn / re-target; `None` on a
    /// same-target resize or when the session/pane is gone. `serde(default)`
    /// keeps the wire compatible with a backend that predates the field.
    #[serde(default)]
    pub pane_command: Option<String>,
}

/// Resize an already-open pty (e.g. when the LLM pane changes size).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyResizeReq {
    pub cols: u16,
    pub rows: u16,
}

/// User keystroke bytes from the LLM pane → the pty. Carried in the
/// envelope payload as a base64 blob to keep the wire JSON-safe;
/// terminal key sequences include escape (`\x1b`) and other binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyWriteReq {
    /// Base64-encoded byte string.
    pub data_b64: String,
}

/// pty → frontend bytes. Same base64 encoding for the same reasons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyEvt {
    pub data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub file: String,
    pub line: i64,
    #[serde(rename = "fn")]
    pub function: String,
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

// ─── Backend-sessions / tmux registry (ADR 0013) ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxSession {
    pub name: String,
    /// Unix epoch seconds.
    pub created: i64,
    pub attached: bool,
    pub windows: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxPane {
    /// `%N`-form pane id; addressing target.
    pub id: String,
    pub session: String,
    pub window_index: u32,
    pub pane_index: u32,
    pub title: String,
    pub command: String,
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxListSessionsRes {
    pub sessions: Vec<TmuxSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxListPanesReq {
    /// Specific session; omit to list panes across the whole server.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxListPanesRes {
    pub panes: Vec<TmuxPane>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxCreateSessionReq {
    pub name: String,
    /// First-window command to run. Omit for a default shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Working directory for the session.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxKillSessionReq {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxCapturePaneReq {
    /// Pane id (`%N`) or `<session>:<window>.<pane>` form.
    pub target: String,
    /// Lines back from the bottom. Capped at 5000 backend-side.
    #[serde(default = "default_capture_lines")]
    pub lines: u32,
}

fn default_capture_lines() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxCapturePaneRes {
    pub text: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCreateReq {
    /// Human-friendly label (project_root basename is fine). Used to
    /// derive the workspace's slug and `sot-be-<slug>` tmux session.
    pub label: String,
    /// Absolute path. The daemon will use this as the workspace's
    /// `--project=` for kernels and as the file walker's root.
    pub project_root: String,
    /// True if the FE should launch claude on first attach to this
    /// workspace's session. `#[serde(default)]` so existing callers /
    /// JSON without the field still deserialize (defaults false).
    #[serde(default)]
    pub autostart_claude: bool,
    /// Which agent to auto-start in the workspace pane (ADR 0031):
    /// "claude" | "codex" | "none". Empty (absent on the wire) derives
    /// from `autostart_claude` for back-compat.
    #[serde(default)]
    pub agent: String,
    /// The sot-comm handle the spawned agent should join as. Optional
    /// on the wire (`#[serde(default)]` → empty string when absent), so
    /// existing callers / JSON without the field still deserialize.
    #[serde(default)]
    pub agent_name: String,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Optional on the wire (`#[serde(default)]` →
    /// empty string when absent).
    #[serde(default)]
    pub task: String,
    // NOTE (ADR 0023 §3): the daemon-boot trigger travels as an extra wire field
    // `boot: bool` on this op's payload, but is intentionally NOT a struct field
    // here — `handle_workspace_create` reads it straight off the raw JSON. Adding
    // it to the struct would force the FE's `WorkspaceCreateReq { … }` literal
    // (transport.rs) to set it, and the FE is frozen during the sot-names rename.
    // serde ignores the unknown field on this typed deserialize, so the contract
    // stays additive. Fold `boot` into the struct once the FE is unfrozen.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCreateRes {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub tmux_session: String,
}

/// `workspace.list` has no fields — the daemon always returns its full
/// in-memory registry. Kept as a struct so future filters (kernel-only,
/// running-only) can land additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceListReq {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceListEntry {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub tmux_session: String,
    /// True if the workspace's `Kernel` handle has been constructed —
    /// i.e. some op has caused the daemon to lazily instantiate it.
    /// Reflects in-memory state only; if the underlying Julia child has
    /// died silently the daemon won't notice until the next op.
    pub kernel_running: bool,
    /// True iff this workspace is the daemon's default (the one ops
    /// resolve to when no `workspace_id` is supplied). The frontend can
    /// use this to mark the row and to avoid switching away from the
    /// implicit anchor.
    pub is_default: bool,
    /// True if the FE should launch claude on first attach to this
    /// workspace's session. `#[serde(default)]` so a daemon that predates
    /// the field (e.g. mid-rollout, not yet restarted) still deserializes.
    #[serde(default)]
    pub autostart_claude: bool,
    /// Which agent this workspace auto-starts (ADR 0031): "claude" |
    /// "codex" | "none". The FE renders a per-agent sigil from this.
    #[serde(default)]
    pub agent: String,
    /// The sot-comm handle the spawned agent should join as. These
    /// (with `task`) let the FE deliver the bootstrap straight off
    /// workspace.list — no fe-inbox correlation. Empty string = unset.
    #[serde(default)]
    pub agent_name: String,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Empty string = unset.
    #[serde(default)]
    pub task: String,
    /// Owning-agent work-state surfaced from the sot-comm registry
    /// (`comm-status.sh` writes `.agents[<agent_name>]`). The daemon reads the
    /// registry fresh on each `workspace.list` and copies these through because
    /// the FE runs on a separate machine/HOME and can't read it directly. One
    /// of "working" | "idle" | "blocked" | "done"; empty string when the
    /// registry / agent / field is absent (no agent_name, no registry yet, …).
    #[serde(default)]
    pub agent_state: String,
    /// One-liner the owning agent set alongside `agent_state` (the `summary`
    /// field in the registry). Empty string when absent.
    #[serde(default)]
    pub agent_summary: String,
    /// ISO8601 timestamp the registry recorded for the state (`status_at`).
    /// Empty string when absent.
    #[serde(default)]
    pub agent_status_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceListRes {
    pub workspaces: Vec<WorkspaceListEntry>,
}

/// `workspace.destroy` request — identify the target workspace by id or
/// slug (the backend's `resolve` accepts either). The default workspace
/// is not destroyable; trying returns an error response with
/// `code = "default_workspace_not_destroyable"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDestroyReq {
    pub workspace_id: String,
}

/// `workspace.destroy` response. Echoes back the slug + label so the
/// frontend status line can identify what got destroyed. `tmux_killed`
/// reflects whether the kill-session call succeeded — `false` is
/// usually "session wasn't running anyway" and not fatal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDestroyRes {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub tmux_killed: bool,
    pub toml_removed: bool,
}

/// `agent.send` request — relay one agent-to-agent message through the
/// daemon. `to == ""` means broadcast to every connection (all machines).
/// The daemon stamps a `ts` and re-emits the body as an `agent.message`
/// evt to every connection (including the sender's, which is harmless —
/// the receiving agent dedups on `ts` if needed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendReq {
    pub from: String,
    pub to: String,
    pub text: String,
}

/// `agent.send` response — a simple ack. `ok` is always true on a
/// successfully-parsed request (the publish is fire-and-forget; a send
/// with no subscribers still acks ok).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSendRes {
    pub ok: bool,
}

/// `fe.command.send` request (ADR 0025) — ask the daemon to drive the
/// frontend(s) with an imperative UI command. The daemon re-emits
/// `{v:1, cmd, args, target}` as an `FE_COMMAND` evt to every connection
/// (mirrors `AGENT_SEND` → `AGENT_MESSAGE`). `cmd` ∈ {"preview", "reveal",
/// "goto_workspace", "goto_mode", "notify"} for v1; `args` is the per-cmd object
/// (e.g. `preview` = `{workspace, path, urgent?}`, `goto_workspace` =
/// `{workspace}`). `target` optionally scopes delivery to one FE by its
/// sot-comm handle; absent = all FEs (the badge floor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandSendReq {
    pub cmd: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `fe.command.send` response — a bare ack. `ok` is true on a parsed request;
/// the FE-command publish is fire-and-forget, so a send with no FE connected
/// still acks ok (mirrors `AgentSendRes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandSendRes {
    pub ok: bool,
}

/// Payload of an `FE_COMMAND` evt (ADR 0025) — one imperative UI command pushed
/// to the frontend(s). `v` is the envelope version (1). The FE parses `{cmd, args}`
/// into an `FeCommand` and dispatches it through `dispatch_fe_command`; `target`
/// (a FE sot-comm handle) scopes which FE acts (`None` = all, the badge floor;
/// `Some` = only that FE, force-show routing). The FE self-filters on `target`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandEvt {
    #[serde(default = "fe_command_version")]
    pub v: u32,
    pub cmd: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

fn fe_command_version() -> u32 {
    1
}

/// Open a Pluto-flavored `.jl` notebook in the backend-supervised
/// Pluto server. Path must be absolute on the backend host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlutoOpenReq {
    pub path: String,
}

/// `pluto.open` response — the per-notebook edit URL the frontend
/// hands to the OS browser-open. URL is loopback-shaped
/// (`http://127.0.0.1:1234/edit?id=<uuid>`); reaching a remote backend
/// requires an SSH `-L 1234:127.0.0.1:1234` tunnel on the launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlutoOpenRes {
    pub url: String,
}

/// Open a video file in the OS browser. Path must be absolute on the backend
/// host (the frontend sends the cursored file's absolute path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoOpenReq {
    pub path: String,
}

/// `video.open` response — the loopback HTTP URL the frontend hands to the OS
/// browser-open. Shaped `http://127.0.0.1:<videoPort><abs-path>`; reaching a
/// remote backend requires an SSH `-L <videoPort>:127.0.0.1:<videoPort>`
/// tunnel on the launcher. ADR 0018.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoOpenRes {
    pub url: String,
}

/// Open the project's built Documenter site in the OS browser. `path` is the
/// cursored file's absolute backend-side path — when it points at a built page
/// under `docs/build`, the response deep-links to it; otherwise (empty or
/// elsewhere) the docs index is opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocsOpenReq {
    pub path: String,
}

/// `docs.open` response — the loopback HTTP URL the frontend hands to the OS
/// browser-open. Shaped `http://127.0.0.1:<docsPort>/<rel>`; reaching a remote
/// backend requires an SSH `-L <docsPort>:127.0.0.1:<docsPort>` tunnel on the
/// launcher. ADR 0024.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocsOpenRes {
    pub url: String,
}

/// Render a Quarto/markdown doc and open it in the OS browser. Path is
/// absolute on the backend host. `execute = false` (the `o` key) renders
/// structure/formatting only — fast, quarto-only, no side effects.
/// `execute = true` (the `O` key) runs code chunks — needs the language
/// kernels on the backend host and is slower.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuartoOpenReq {
    pub path: String,
    pub execute: bool,
}

/// `quarto.open` response — the rendered self-contained HTML, base64-encoded
/// (Quarto's `--embed-resources` inlines all CSS/JS/images so a single blob
/// is enough). The frontend writes it to a temp `.html` and hands it to the
/// OS browser, reusing the `text/html` preview's open path — no HTTP server
/// or port-forward needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuartoOpenRes {
    pub html_base64: String,
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

// ─── Server monitoring (ADR 0020) ───────────────────────────────────────

/// One GPU's metrics within a sample. `index` is the GPU's ordinal on its host
/// (a shared multi-GPU host has two). `name` rides only when known — typically the first
/// sample of a series — to spare the wire on every subsequent point. `temp_c`
/// and `power_w` are optional: the data plane (Netdata already collects them)
/// can start populating them with no protocol change (ADR 0020 §4 — util + mem
/// are the defaults, temp/power are one field away).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuSample {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub util_pct: f32,
    pub mem_pct: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f32>,
}

/// One process in a sample's top-by-CPU list: command name, owning user, and
/// its CPU share. `cpu_pct` is instantaneous (utime+stime delta across the
/// sampler's tick interval, the same accounting `top` uses) and, like top's
/// irix mode, can exceed 100 for a multithreaded process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcSample {
    pub name: String,
    pub user: String,
    pub cpu_pct: f32,
}

/// A single observation: host CPU + RAM at `ts`, plus one entry per GPU. `ts`
/// is epoch seconds (f64 to allow sub-second cadence). The host is not repeated
/// here — it is carried by the enclosing `HostSeries` / `HostLatest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSample {
    pub ts: f64,
    pub cpu_pct: f32,
    pub ram_pct: f32,
    /// Host CPU logical core count and total RAM in GiB. Static per host, so
    /// the backend may send them on every sample or only the first; the
    /// frontend caches the last-known value and renders the percentage with
    /// its absolute capacity (e.g. `32c 10%`, `128G 3%`). Optional so an
    /// older backend that omits them still renders the bare percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_total_gb: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuSample>,
    /// Top processes by CPU at sample time (typically 3). Live ticks carry
    /// them; downsampled history buckets drop them (instantaneous data does
    /// not average). Defaulted so older backends interop unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_procs: Vec<ProcSample>,
}

/// A downsampled history window for one host. `step_s` is the resolution the
/// series came back at (which Netdata tier), so the frontend can label the axis
/// and decide whether to request finer/coarser data on rescale. `stale` marks a
/// host that returned no data for the window — the frontend renders a gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSeries {
    pub host: String,
    pub step_s: f64,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub samples: Vec<MonitorSample>,
}

/// `monitor.subscribe` — start this connection's live metrics stream. The
/// initial window fill is a separate `monitor.history` call (keeps subscribe
/// pure lifecycle), so the same path serves the first paint and every rescale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSubscribeReq {
    /// Desired tick cadence in seconds; clamped backend-side to what Netdata
    /// actually produces (>= 1s).
    #[serde(default = "default_interval_s")]
    pub interval_s: f64,
}

/// `monitor.subscribe` response — echoes the cadence the backend will actually
/// stream at (after clamping) so the frontend can size its live ring buffer,
/// plus the host roster (in display order) so empty panels can be laid out
/// before the first tick arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSubscribeRes {
    pub interval_s: f64,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// `monitor.unsubscribe` — stop this connection's live stream. Empty payload;
/// response is a bare ack (`{}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorUnsubscribeReq {}

/// `monitor.history` — fetch a window for one or all hosts from the Netdata
/// parent's tiers, downsampled to ~`points`. `until` defaults to now; the
/// window spans `window_s` back from it. `host = None` returns every host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorHistoryReq {
    pub window_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<f64>,
    #[serde(default = "default_points")]
    pub points: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorHistoryRes {
    pub hosts: Vec<HostSeries>,
}

/// One host's latest sample inside a `monitor.tick`. `sample` is absent when
/// the host went unreachable for the interval (`stale = true`) — the frontend
/// advances the axis and draws a gap rather than holding the last value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostLatest {
    pub host: String,
    #[serde(default)]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<MonitorSample>,
}

/// Payload of a `monitor.tick` evt — one fresh sample per host, pushed at the
/// subscribed cadence (ADR 0020 §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorTickEvt {
    pub hosts: Vec<HostLatest>,
}

fn default_interval_s() -> f64 {
    1.0
}

fn default_points() -> u32 {
    240
}

// ─── Auto-update (ADR 0030 §4, Phase C) ─────────────────────────────────

/// `update.check` request — no fields today (the backend always checks its
/// configured repo against its own embedded version). Kept as a struct so a
/// future forced-channel / forced-repo override can land additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckReq {}

/// `update.check` response. `current` is the running product version
/// (`app_version()`); `latest` is the newest release's version with the tag's
/// leading `v` stripped (empty when the check couldn't run). `update_available`
/// is true only when `latest` is a strictly newer semver than `current`.
/// `staged` is true once the platform asset for `latest` has been downloaded,
/// sha256-verified, and unpacked into the staging dir. `status` is a
/// human/structured string: `"ok"`, `"disabled: dev build"`,
/// `"disabled: update mode off"`, or `"check unavailable: <why>"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckRes {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub staged: bool,
    pub status: String,
}

#[cfg(test)]
mod hello_version_tests {
    use super::{HelloReq, HelloRes};

    #[test]
    fn legacy_hello_req_defaults_to_preversioning() {
        // A pre-versioning frontend sends a HelloReq without the ADR 0030
        // `protocol` / `app_version` fields. `#[serde(default)]` must fill them
        // with the "pre-versioning peer" sentinels (0 / "") so the backend gate
        // can recognize the peer as legacy rather than failing the parse.
        let json = r#"{"client_id":"c1","last_seen_revision":7}"#;
        let req: HelloReq = serde_json::from_str(json).expect("legacy HelloReq deserializes");
        assert_eq!(req.client_id, "c1");
        assert_eq!(req.last_seen_revision, 7);
        assert_eq!(req.protocol, 0, "missing protocol → 0 (pre-versioning)");
        assert_eq!(req.app_version, "", "missing app_version → empty");
    }

    #[test]
    fn legacy_hello_res_defaults_to_preversioning() {
        // Symmetric guard for the response: a pre-versioning backend's HelloRes
        // lacks the two fields; the newer frontend must still deserialize it,
        // seeing protocol == 0 (which it warns about but tolerates).
        let json = r#"{"session_id":"s1","revision":3,"snapshot_pending":false}"#;
        let res: HelloRes = serde_json::from_str(json).expect("legacy HelloRes deserializes");
        assert_eq!(res.session_id, "s1");
        assert_eq!(res.protocol, 0, "missing protocol → 0 (legacy backend)");
        assert_eq!(res.app_version, "");
    }

    #[test]
    fn versioned_hello_req_round_trips() {
        let req = HelloReq {
            client_id: "c2".into(),
            session_id: None,
            last_seen_revision: 0,
            token: None,
            protocol: 2,
            app_version: "0.2.0-dev+abc".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: HelloReq = serde_json::from_str(&json).unwrap();
        assert_eq!(back.protocol, 2);
        assert_eq!(back.app_version, "0.2.0-dev+abc");
    }
}
