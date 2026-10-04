//! Every event a host's connection hands the UI thread; the caller tags each with its dial `HostKey`.

use super::*;

/// Messages the transport task pushes back to the GPU thread.
#[derive(Debug)]
pub enum IncomingEvt {
    Connected {
        session_id: String,
        revision: u64,
        /// The backend's declared host (`HelloRes.host`, ADR 0046 decision
        /// 1 — resolved once by `sot_log::state_dir::host_name()` on that
        /// side) — `Some("myhost")` when a backend reports itself, `None`
        /// for older backends. `HostKey` (the dial label this connection
        /// is tagged with, `hosts.toml`-configured) is NEVER re-homed to
        /// this value — the GPU thread records it separately, for display
        /// only (`crate::gpu::host_label`, the one display projection).
        host: Option<String>,
        /// `--project-root` the backend was started with, so the chrome
        /// can show "myhost:Ship of Tools" rather than just the host.
        project_root: Option<String>,
        /// The daemon advertised the ADR-0035 TCP proxy (`HelloRes.proxy`).
        /// A remote FE arms its lazy loopback proxy listeners only when this
        /// is true; `false` for older daemons (falls back to the launcher's
        /// per-port ssh forwards exactly as before).
        proxy: bool,
        /// C3 as amended §5: which transport actually connected —
        /// `ResolvedDial::Local` for the pipe, `ResolvedDial::Ssh(recipe)`
        /// for the ssh child, carrying the exact recipe it spawned. The
        /// proxy arms on `proxy && !matches!(resolved, ResolvedDial::Local)`
        /// (`gpu.rs`) — keyed on the transport that CONNECTED, not the CLI
        /// shape.
        /// Replaces the former separate `remote: bool` / `tcp_peer:
        /// Option<SocketAddr>` pair: `remote` was literally `via_tcp`, and
        /// the two values that gate proxying and that a second connection
        /// dials must be ONE value so they cannot disagree (the amendment's
        /// own blocker: as two fields, an ssh control connection is remote
        /// and not tcp, so the old `None if !remote => Local` arm recorded
        /// `Local` for every ssh host and disarmed its proxy).
        resolved: ResolvedDial,
        /// The backend's product version (`HelloRes::app_version`), e.g.
        /// `0.5.8` or `0.5.8-dev+a1b2c3d`. Painted next to the FE's own
        /// version on the bottom chrome edge so a running FE/BE skew is
        /// visible continuously — not only when it is hard enough to trip
        /// the ADR 0030 §2 protocol-mismatch overlay. Empty for a
        /// pre-versioning backend that doesn't send the field.
        backend_version: String,
    },
    Disconnected {
        reason: String,
    },
    /// The backend refused the handshake because the FE↔BE wire-contract
    /// protocol versions differ (ADR 0030 §2). Unlike a transient
    /// `Disconnected`, this is a hard, self-diagnosing skew: the chrome shows a
    /// persistent blocking full-pane "update needed" message carrying both
    /// sides' versions + protocols + the dev fix hint. `message` is the
    /// pre-formatted multi-line body to display; its first line is also the
    /// agent pane's state ("frontend out of date" / "daemon out of date").
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
        /// Per-(host, workspace) request generation, echoed from
        /// `OutgoingReq::ProjectScan` — `kernel.request` runs off-loop, so
        /// two scans fired close together for the same workspace can
        /// complete in either order; the chrome drops one whose generation
        /// isn't the latest it issued for that (host, workspace).
        generation: u64,
    },
    /// `concept.read` reply for `target`. `content` is the raw markdown
    /// (including YAML frontmatter) if `exists`; empty otherwise. Used by
    /// the chrome to show the annotation under the selected tree node and
    /// to drive the drift badge once `synced_against`-vs-AST-hash compare
    /// lands.
    ConceptRead {
        target: String,
        /// Switch-latency Phase 1: the workspace this read was fired for,
        /// echoed from `PendingKind::ConceptRead` — previously dropped
        /// here, which meant a late reply from a workspace the chrome had
        /// since left could be mistaken for the active one whenever the
        /// `target` string happened to coincide (two projects annotating
        /// the same relative path). Paired with `generation`, below.
        workspace_id: Option<String>,
        exists: bool,
        content: String,
        /// Same mechanism as `IncomingEvt::Preview::generation` — the
        /// concept/annotation slot's request generation, so an
        /// out-of-order `concept.read` reply for a target the cursor has
        /// since moved away from (and back to) can't be mistaken for the
        /// current one.
        generation: u64,
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
    /// `dir.create` reply (FE Ctrl+N with a trailing `/`): happy path or an
    /// `{error, code}` failure — see `DirCreateResult`. The chrome matches
    /// `node_id` against `pending_created_node_id`, refreshes the parent dir,
    /// and clears the pending marker on either outcome.
    DirCreateDone {
        node_id: String,
        result: DirCreateResult,
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
        /// Switch-latency Phase 1: the preview slot's request generation,
        /// echoed verbatim from the `PendingKind` this reply resolved
        /// (`0` for the connect-time preamble fetch, which has no
        /// `PendingKind` to source one from). The chrome drops any reply
        /// whose generation is behind the latest one it has issued for the
        /// preview slot — otherwise a `preview.get`/`preview.set_scale`
        /// answered slowly and out of order (a daemon can now reply
        /// out-of-order per-connection) could overwrite what a newer
        /// cursor move already asked for, even though `workspace_id` alone
        /// still matches.
        generation: u64,
    },
    /// A plain `preview.get` reply came back as an ERROR envelope
    /// (`{"error", "code"}`) rather than a `PreviewGetRes` — most notably
    /// `code: "kernel_unavailable"`: the Julia kernel is unavailable and
    /// this file type has no sane bytes-level fallback (HDF5, video, PDF).
    /// Same `"{code}: {err}"` status-line convention as
    /// `ScaleSetFailed`/`ImageCropFailed`. Carries the same ownership pair
    /// those don't need but this DOES (a preview request can be superseded
    /// by a workspace switch or a different file selection before its
    /// error arrives) — the consumer applies `reply_is_current` exactly
    /// like the success path (`IncomingEvt::Preview`) does, so a stale
    /// failure can never overwrite what's current.
    PreviewGetFailed {
        node_id: Option<String>,
        workspace_id: Option<String>,
        generation: u64,
        message: String,
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
    /// A `figure.get` reached a terminal failure without ever producing
    /// bytes — either the reply payload didn't parse as `PreviewGetRes`
    /// (which covers both an explicit `{error, code}` envelope and any
    /// other malformed reply, since `mime`/`blob` are required fields), or
    /// the connection dropped before any reply arrived (`PendingGuard`
    /// flushes outstanding `FigureGet` entries on `run_protocol` exit).
    /// Field report: before this event existed, either case left `url`
    /// stuck in `figure_pending` forever — `dispatch_pending_figures`
    /// never retries anything already pending. The chrome reaches the
    /// same terminal `figure_failed` state this drives for a decode
    /// failure, so the layout still collapses instead of holding an empty
    /// reservation.
    FigureGetFailed {
        url: String,
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
    /// ADR 0042 slice L1b: a `pty.open` came back refused with
    /// `code: "attach_direct"` — the row THIS REQUEST targeted (mirrors
    /// `PtyOpenReq.target`, carried through `PendingKind::PtyOpen`) is
    /// actually a capsule workspace (a stale `workspace.list` cache, or
    /// the reconnect re-fire racing a runtime flip). `target` is what
    /// this specific reply is ABOUT — never assume it's whatever row is
    /// currently selected, which a later switch can have changed by the
    /// time a stale reply lands (L1b fix 1). ADR 0045 decision 1: the
    /// chrome now attaches every capsule row through its own daemon's
    /// `lane.connect` bridge, so this reply no longer needs a state dir —
    /// the daemon keeps emitting one on the wire (until the next
    /// `PROTOCOL_VERSION` bump) but it is simply ignored here.
    PtyAttachDirect {
        target: Option<String>,
    },
    /// A `pty.open` reply came back neither `attach_direct` nor parseable
    /// as one — this build's daemon has no other success case to offer
    /// (see `PtyAttachDirect`'s own doc), so anything else is a failure
    /// the chrome must surface rather than silently drop. `target` mirrors
    /// `PtyOpenReq.target` (via `PendingKind::PtyOpen`), same as
    /// `PtyAttachDirect`; `error` is the reply's own `code` field, or
    /// `"unsupported daemon reply"` when the payload carries none —
    /// routed through the pane's existing persistent-reason path
    /// (`pane_dial_error`) so a pane left `Pending` by this reply says why
    /// instead of sitting mute with buffered keystrokes.
    PtyOpenFailed {
        target: Option<String>,
        error: String,
    },
    /// Raw event we don't handle in the spike yet — kept for visibility.
    Event {
        op: String,
        #[allow(dead_code)]
        payload: Value,
    },
    /// `tmux.list_panes` reply for the queried session (or for the whole
    /// server when `session: None` was sent). ADR 0042 L2a codex review
    /// deletions: the sibling `TmuxSessions`/`TmuxSessionCreated`/
    /// `TmuxSessionKilled` replies (to `tmux.list_sessions`/
    /// `tmux.create_session`/`tmux.kill_session`) had no production
    /// sender — ADR 0014 moved Sessions mode onto the daemon's workspace
    /// registry instead of scanning tmux, before this spike-era plumbing
    /// was ever wired up.
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
    /// `accounts.list` reply (owner-simplified brief, 2026-09-15). Empty
    /// on an old daemon (no handler for the op) or a daemon reporting only
    /// "default" — either way the new-session prompt hides the account
    /// choice entirely.
    AccountsList {
        accounts: Vec<AccountInfo>,
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
