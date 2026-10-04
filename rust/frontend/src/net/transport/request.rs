//! Every request the UI can send a host's connection.

use super::*;

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
    /// only knows "files".
    TreeRoot {
        mode: String,
        /// ADR 0014 workspace routing. Same shape as TreeChildren.
        workspace_id: Option<String>,
    },
    /// Flip the backend's per-workspace Files-mode "show hidden files" flag
    /// (`nav.toggle_hidden`, ADR: `.` keybind). The backend bumps
    /// `tree.invalidate`; the caller (`toggle_hidden_files`, ui/nav/files/listing.rs) re-fetches `tree.root` right
    /// after on the same ordered connection so the new visibility shows up.
    /// The response carries the new state but the FE ignores it (the re-fetch
    /// is authoritative), so no PendingKind is stamped.
    ToggleHidden { workspace_id: Option<String> },
    /// Explicit "this connection's view is now `workspace_id`" signal
    /// (`workspace.activate`, `None` = default workspace). Fired
    /// UNCONDITIONALLY as the very first wire action of `switch_to_workspace`
    /// (ui/session/switch.rs) — including a UI-cache hit that fires no other request at
    /// all — and once more on reconnect, right after `hello` succeeds
    /// (`IncomingEvt::Connected`'s resume handling). Replaces inferring the
    /// active workspace daemon-side from whichever op's `workspace_id`
    /// happened to arrive next, which could leave the daemon's
    /// `preview.changed` fan-out filter stuck on a stale workspace
    /// indefinitely (Codex review). Response echoes back the canonical id
    /// the daemon resolved to, but the FE doesn't act on it today — no
    /// PendingKind is stamped, mirroring `ToggleHidden` above.
    ///
    /// `read` is `true` only for the two person-driven view switches
    /// (Sessions-Enter, Shift+Left/Right cycling) — it tells the daemon a
    /// person looked at this workspace, which clears a `done` row's blue
    /// (ADR 0044). Every other caller (agent-driven switches, `create`'s
    /// auto-switch, the destroy bounce, reconnect re-announce) sends
    /// `false`.
    WorkspaceActivate { workspace_id: Option<String>, read: bool },
    /// "A person is providing real input right now" (`fe.presence`,
    /// 2026-09-08 review rework, design point A). Fired ONLY from the
    /// winit `window_event` handler's real `KeyboardInput`/`MouseInput`
    /// arms, throttled there to at most one per `PRESENCE_THROTTLE` — never
    /// from command-file dispatch, capture-mode simulation, or any other
    /// automated path. `ui/session/presence.rs`'s `report_presence` sends this one PER
    /// CONNECTED HOST (`send_to`, not `send`) under that one throttle: a
    /// person is present for every daemon this frontend is attached to,
    /// not only whichever host is active right now — otherwise a daemon
    /// the person hasn't LOOKED at recently, but whose commands still
    /// route through here, would see this frontend go stale. Empty
    /// payload, fire-and-forget: no `PendingKind` is stamped, same idiom
    /// as `ToggleHidden`/`WorkspaceActivate` above. The daemon is the one
    /// place that decides "the active frontend" from this signal
    /// (`Clients::touch_person_input`) — the FE never infers its own
    /// activity from op traffic, which is exactly the false-positive/
    /// false-negative pair the review found in the daemon-side-inference
    /// design this replaces.
    FePresence,
    /// Declare the sot-comm handles this box's OWN daemon owns, with
    /// their state (session-listing brief decision 2), on a connection
    /// OTHER than that owning daemon's own — see `ui/events.rs`'s
    /// `IncomingEvt::Workspaces`/`Connected` arms for who sends this and
    /// when. Fire-and-forget, same idiom as `FePresence` above: no
    /// `PendingKind`, an older daemon's unknown-op error is silently
    /// dropped by the unmatched-id fallthrough.
    FeSessions(Vec<sot_protocol::DeclaredSession>),
    /// Ask the kernel to scan the project's package source tree
    /// (`<project_root>/src/<PkgName>.jl` + everything it `include`s)
    /// and return a nested {modules → types/functions/submodules}
    /// view. Drives the unified Modules+Types nav mode.
    ProjectScan { workspace_id: Option<String>, generation: u64 },
    /// Fetch the `.concept/<target>.md` annotation for `target`. Response
    /// surfaces as `IncomingEvt::ConceptRead`.
    ConceptRead {
        target: String,
        workspace_id: Option<String>,
        /// Switch-latency Phase 1: the caller's concept-slot request
        /// generation (its own monotonic "requests fired for this slot"
        /// counter, incremented before this call) — stamped into
        /// `PendingKind::ConceptRead` unchanged and echoed back on the
        /// reply so the caller can tell a stale answer from the latest
        /// one it asked for.
        generation: u64,
    },
    /// Persist `content` as the `.concept/<target>.md` annotation. When
    /// `expected_ast_hash` is `Some`, the backend gates the write on
    /// the on-disk `synced_against` matching (Linux's `4ebca35`) and
    /// returns a `stale_write` error otherwise. The chrome should always
    /// send `expected_ast_hash` for an edit-then-save flow so the gate
    /// is engaged.
    ConceptWrite {
        target: String,
        content: String,
        expected_ast_hash: Option<String>,
        workspace_id: Option<String>,
    },
    /// Read a source file's full text for the editor. Reply →
    /// `IncomingEvt::FileRead`. Distinct from `preview.get` (kernel-rendered).
    FileRead {
        node_id: String,
        workspace_id: Option<String>,
    },
    /// Persist editor content. `expected_version` (from the matching
    /// `FileRead`) engages the optimistic-concurrency gate; `None` forces.
    /// Reply → `IncomingEvt::FileWriteDone`.
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
    /// Create a directory from Files-mode nav (Ctrl+N, a name ending in `/`).
    /// Non-recursive server-side — the parent must already exist. Reply →
    /// `IncomingEvt::DirCreateDone`.
    DirCreate {
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
        /// Switch-latency Phase 1: caller's preview-slot request
        /// generation. See `ConceptRead::generation`.
        generation: u64,
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
        /// Switch-latency Phase 1: shares the preview slot's generation
        /// counter with `PreviewGet` — both install through the same
        /// `IncomingEvt::Preview` consumer.
        generation: u64,
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
    DirectoryList { path: String, include_hidden: bool },
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
        /// Per-session accounts (owner-simplified brief, 2026-09-15): the
        /// login directory name the picker resolved, or `None` for the
        /// agent's default directory. Sent through to
        /// `sot_protocol::WorkspaceCreateReq.account` verbatim.
        account: Option<String>,
    },
    /// Enumerate registered workspaces on the daemon (ADR 0014).
    /// Replaces the `tmux.list_sessions` prefix-filter as the source of
    /// truth for Sessions mode rows. Response surfaces as
    /// `IncomingEvt::Workspaces` carrying the full registry view.
    WorkspaceList,
    /// Per-session accounts (owner-simplified brief, 2026-09-15): ask the
    /// daemon which login directories it can see for a row it would spawn
    /// here. Fired when the new-session picker opens, targeting the
    /// picker's own host (`send_to`) — never the frontend's own disk, since
    /// the login must exist where the session actually runs. Response
    /// surfaces as `IncomingEvt::AccountsList`; an old daemon with no
    /// handler for `accounts.list` fails to parse as `AccountsListRes` and
    /// is treated as an empty list (default-only), not an error.
    AccountsList,
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
    /// Relay one FE-originated notification through the daemon's `agent.send`
    /// broadcast (re-emitted to every connection as an `agent.message` evt).
    /// Today's only producer is the ADR-0025 `preview_roi_applied` echo:
    /// `fe.command.send` is fire-and-forget, so a `preview --roi`'s effective
    /// (post-clamp) rect can't ride that ack and comes back through this relay
    /// instead. `text` is the event JSON; `to == ""` broadcasts.
    AgentSend {
        from: String,
        to: String,
        text: String,
    },
}

/// Write one outgoing request: one arm per `OutgoingReq` variant.
pub(super) async fn send_request<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    req: OutgoingReq,
) -> Result<()> {
    match req {
        OutgoingReq::TreeChildren { parent_id, workspace_id } => send_tree_children(&mut tx, pending, id, parent_id, workspace_id).await?,
        OutgoingReq::TreeRoot { mode, workspace_id } => send_tree_root(&mut tx, pending, id, mode, workspace_id).await?,
        OutgoingReq::ToggleHidden { workspace_id } => send_toggle_hidden(&mut tx, id, workspace_id).await?,
        OutgoingReq::WorkspaceActivate { workspace_id, read } => send_workspace_activate(&mut tx, id, workspace_id, read).await?,
        OutgoingReq::FePresence => send_fe_presence(&mut tx, id).await?,
        OutgoingReq::FeSessions(sessions) => send_fe_sessions(&mut tx, id, sessions).await?,
        OutgoingReq::ProjectScan { workspace_id, generation } => send_project_scan(&mut tx, pending, id, workspace_id, generation).await?,
        OutgoingReq::MarkdownTokenize { lang, source_hash, source } => send_markdown_tokenize(&mut tx, pending, id, lang, source_hash, source).await?,
        OutgoingReq::ConceptRead { target, workspace_id, generation } => send_concept_read(&mut tx, pending, id, target, workspace_id, generation).await?,
        OutgoingReq::MathRender { latex, display } => send_math_render(&mut tx, pending, id, latex, display).await?,
        OutgoingReq::ImageCrop { node_id, x, y, w, h, workspace_id } => send_image_crop(&mut tx, pending, id, node_id, x, y, w, h, workspace_id).await?,
        OutgoingReq::ConceptWrite { target, content, expected_ast_hash, workspace_id } => send_concept_write(&mut tx, pending, id, target, content, expected_ast_hash, workspace_id).await?,
        OutgoingReq::FileRead { node_id, workspace_id } => send_file_read(&mut tx, pending, id, node_id, workspace_id).await?,
        OutgoingReq::FileWrite { node_id, content, expected_version, workspace_id } => send_file_write(&mut tx, pending, id, node_id, content, expected_version, workspace_id).await?,
        OutgoingReq::FileDelete { node_id, workspace_id } => send_file_delete(&mut tx, pending, id, node_id, workspace_id).await?,
        OutgoingReq::DirCreate { node_id, workspace_id } => send_dir_create(&mut tx, pending, id, node_id, workspace_id).await?,
        OutgoingReq::FileParse { path, workspace_id } => send_file_parse(&mut tx, pending, id, path, workspace_id).await?,
        OutgoingReq::PreviewGet { node_id, workspace_id, page, fit_w, fit_h, generation } => send_preview_get(&mut tx, pending, id, node_id, workspace_id, page, fit_w, fit_h, generation).await?,
        OutgoingReq::PreviewSetScale { node_id, nm_per_px, workspace_id, generation } => send_preview_set_scale(&mut tx, pending, id, node_id, nm_per_px, workspace_id, generation).await?,
        OutgoingReq::FigureGet { url, node_id, workspace_id } => {
            tracing::debug!(%url, %node_id, ?workspace_id, id, "→ figure.get (preview.get)");
            send_figure_get(&mut tx, pending, id, url, node_id, workspace_id)
                .await?;
        }
        OutgoingReq::FunctionMethods { module, name, workspace_id } => send_function_methods(&mut tx, pending, id, module, name, workspace_id).await?,
        OutgoingReq::ReplEval { eval_id, code, mode, workspace_id } => send_repl_eval(&mut tx, pending, id, eval_id, code, mode, workspace_id).await?,
        OutgoingReq::ReplInterrupt { workspace_id } => send_repl_interrupt(&mut tx, id, workspace_id).await?,
        OutgoingReq::PtyOpen { cols, rows, target, user_switch } => send_pty_open(&mut tx, pending, id, cols, rows, target, user_switch).await?,
        OutgoingReq::DirectoryList { path, include_hidden } => send_directory_list(&mut tx, pending, id, path, include_hidden).await?,
        OutgoingReq::WorkspaceCreate { label, project_root, autostart_claude, agent, account } => send_workspace_create(&mut tx, pending, id, label, project_root, autostart_claude, agent, account).await?,
        OutgoingReq::WorkspaceList => send_workspace_list(&mut tx, pending, id).await?,
        OutgoingReq::AccountsList => send_accounts_list(&mut tx, pending, id).await?,
        OutgoingReq::WorkspaceDestroy { workspace_id } => send_workspace_destroy(&mut tx, pending, id, workspace_id).await?,
        OutgoingReq::PlutoOpen { path } => send_pluto_open(&mut tx, pending, id, path).await?,
        OutgoingReq::VideoOpen { path } => send_video_open(&mut tx, pending, id, path).await?,
        OutgoingReq::DocsOpen { path } => send_docs_open(&mut tx, pending, id, path).await?,
        OutgoingReq::QuartoOpen { path, execute } => send_quarto_open(&mut tx, pending, id, path, execute).await?,
        OutgoingReq::ReplRunFile { eval_id, path, fresh, workspace_id } => send_repl_run_file(&mut tx, pending, id, eval_id, path, fresh, workspace_id).await?,
        OutgoingReq::FileDownload { path, dest } => send_file_download(&mut tx, pending, id, path, dest).await?,
        OutgoingReq::FileUpload { dir, name, offset, total, eof, bytes } => send_file_upload(&mut tx, pending, id, dir, name, offset, total, eof, bytes).await?,
        OutgoingReq::MonitorSubscribe => send_monitor_subscribe(&mut tx, pending, id).await?,
        OutgoingReq::MonitorUnsubscribe => send_monitor_unsubscribe(&mut tx, id).await?,
        OutgoingReq::AgentSend { from, to, text } => send_agent_send(&mut tx, id, from, to, text).await?,
        OutgoingReq::MonitorHistory { window_s, points, until, host } => send_monitor_history(&mut tx, pending, id, window_s, points, until, host).await?,
    }
    Ok(())
}
