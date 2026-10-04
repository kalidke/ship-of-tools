// ops/ — typed payloads for the M1-spike ops.
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
    /// Persist a user-entered physical scale for a raster preview (ADR 0034 §5,
    /// live entry): writes an `<image>.scale.json` sidecar carrying the
    /// per-ORIGINAL-pixel `physical_scale`, then re-emits the preview (same
    /// `PreviewGetRes` envelope, with the F1 downsample rescale applied) so the
    /// FE renders the scalebar without guessing the served/source ratio.
    pub const PREVIEW_SET_SCALE: &str = "preview.set_scale";
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
    /// frontend exercise kernel features (`file.parse`, …)
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
    /// Create a directory from Files-mode nav (Ctrl+N with a trailing `/` in
    /// the typed name). Non-recursive — the parent must already exist — so a
    /// typo in a multi-level name fails loudly instead of silently creating
    /// intermediate directories.
    pub const DIR_CREATE: &str = "dir.create";
    pub const REPL_EVAL: &str = "repl.eval";
    /// Run a `.jl` file either in the persistent REPL (`fresh:false`,
    /// via `include`) or in a fresh `julia` subprocess (`fresh:true`).
    /// Project is auto-detected by walking up from the file path. Same
    /// frame stream shape as `repl.eval`.
    pub const REPL_RUN_FILE: &str = "repl.run_file";
    pub const REPL_INTERRUPT: &str = "repl.interrupt";
    /// Authoritative request/response run: execute a `.jl` file (or a code
    /// chunk) in a workspace's persistent REPL and return the COLLECTED output
    /// as one response (`ReplExecuteRes`) — the "session grabs the output"
    /// path (ADR 0033). Unlike `repl.eval` / `repl.run_file` (immediate ack +
    /// lossy `repl.frame` broadcast), this blocks until the shim's terminal
    /// `res`, gathering frames off a dedicated per-run collector in the
    /// supervisor (never the 256-slot broadcast bus, which drops frames under
    /// a println flood). Text is bounded and images spill to
    /// `<ws>/.sot/runs/<run_id>/` so the response never exceeds the 1 MiB
    /// envelope cap. Non-destructive: runs `include`/eval in the REPL's
    /// current project without resetting it. Frames still broadcast, so a
    /// front-end can display the run too (`origin`-tagged, ADR 0033 phase 2).
    pub const REPL_EXECUTE: &str = "repl.execute";
    /// Server→client push: one REPL output frame, streamed as it is produced
    /// (ADR 0009 phase-2). Replaces the synchronous-collect model where every
    /// frame rode the `repl.eval` / `repl.run_file` *response*. The kernel now
    /// runs the eval in a task and emits each frame immediately; the backend
    /// forwards it as a `repl.frame` evt. Payload is `ReplFrameEvt`
    /// (`{eval_id, workspace_id?, frame}`). The `done` frame is terminal — no
    /// further frames for that `eval_id` follow. The `repl.eval` /
    /// `repl.run_file` response is now a terminal ack (empty `frames`).
    pub const REPL_FRAME: &str = "repl.frame";
    /// Attach to a row's agent pane. Every row is a capsule (ADR 0046):
    /// the daemon always answers with an `attach_direct` refusal
    /// carrying the `state_dir` the frontend attaches to directly — see
    /// `PtyAttachDirect`. No runtime on this branch ever answers with a
    /// size-confirmation success case.
    pub const PTY_OPEN: &str = "pty.open";
    /// Fire-and-forget keystroke bytes to THIS connection's own pty.
    /// Unhandled on this branch (there is no runtime left that answers
    /// `pty.open` with anything but `attach_direct`), kept only because
    /// a backend integration test still exercises firing it as an
    /// arbitrary unhandled op (`navigation_and_typing_ops_never_stamp_
    /// presence_only_fe_presence_does`).
    pub const PTY_WRITE: &str = "pty.write";
    /// ADR 0042 amendment (2026-09-07), "a session types into and reads a
    /// sibling row": a session types into ANOTHER row's pane by
    /// `workspace_id`. Answered — a caller with no pane to look at needs
    /// the outcome. Request `PtyInputReq`, response `PtyInputRes` or a
    /// typed error (`unknown_workspace`, `capsule_not_ready`,
    /// `capsule_input_failed`, `capsule_input_unknown`, `input_not_text`,
    /// `runtime_not_available`, `bad_origin`). A capsule row is served by
    /// a HEADLESS ATTACH CLIENT that takes the pen only long enough to
    /// type and never resizes it (`capsule_workspace::headless`) — ADR
    /// 0041's take-on-first-input semantics, applied to a second kind of
    /// client.
    pub const PTY_INPUT: &str = "pty.input";
    /// ADR 0042 amendment (2026-09-07): the CURRENT screen of a row named by
    /// `workspace_id` — no scrollback, no history (the record and a future
    /// Issue-B own those). Request `PtyScreenReq`, response `PtyScreenRes`
    /// or a typed error (same vocabulary as `PTY_INPUT`, minus the
    /// input-only codes, plus `capsule_screen_failed`). A tmux row reads its
    /// pane's VISIBLE rows (never `tmux.capture_pane`'s scrollback) plus
    /// geometry/cursor; a capsule row attaches as a WATCHER (never takes the
    /// pen) and reads the same checkpoint the frontend's own client would.
    pub const PTY_SCREEN: &str = "pty.screen";
    /// Server-pushed evt: a file under the project root changed on disk.
    /// Carries `{path, node_id?, kind}` (kind ∈ "modified" | "created" |
    /// "removed"). Frontend re-fetches preview if the path matches a
    /// cursored / pinned node; otherwise ignores. Emitted by the
    /// notify-based watcher and bumped on the session ring so a reconnecting
    /// client catches changes that happened while it was away.
    pub const PREVIEW_CHANGED: &str = "preview.changed";
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
    /// Move a LIVE capsule row to another account and resume the same
    /// conversation there (ADR 0046 decision 6). Request is
    /// `WorkspaceReauthReq {workspace_id, account, resume}`; the reply is
    /// `{"code":"reauth_accepted"}` written BEFORE the row's current leg
    /// is ended, because the caller is the session being replaced. Every
    /// refusal leaves the row exactly as it was, and answers with the
    /// accounts this daemon can see — empty only for the two refusals
    /// that precede discovery itself (a payload that will not parse, a
    /// home that will not resolve). The row record is mutated in exactly
    /// one field (`account`): id, slug, root and declared handle all
    /// survive, so the replacement leg is the same row, same comm
    /// identity, different login.
    pub const WORKSPACE_REAUTH: &str = "workspace.reauth";
    /// Accounts brief (v0.6.0): enumerate every account this daemon's
    /// own home discovers RIGHT NOW — nothing declared, nothing cached.
    /// Empty request payload; response is `AccountsListRes`.
    pub const ACCOUNTS_LIST: &str = "accounts.list";
    /// Client→daemon: explicitly names the workspace THIS CONNECTION is now
    /// viewing/acting in. Sent by the frontend's single "switch chrome"
    /// entry point (`switch_to_workspace`, ui/session/switch.rs) UNCONDITIONALLY as the
    /// very first wire action of any workspace switch — including a UI-cache
    /// hit that fires no other request at all, and a switch back to the
    /// default workspace (`workspace_id: None`, same convention as
    /// `TreeRootReq`) — so the daemon's per-connection "active workspace"
    /// (used to fan out `preview.changed` only to a connection whose view
    /// can use it) is never left to be inferred from whatever unrelated op
    /// happens to arrive next. Also sent once on reconnect, right after
    /// `hello` succeeds, so a resumed connection's active workspace is known
    /// before the first tree/preview request lands. Request is
    /// `WorkspaceActivateReq`; response `WorkspaceActivateRes` echoes back
    /// the CANONICAL id actually resolved (`None` if `workspace_id` didn't
    /// resolve — a stale id, or a race with a concurrent destroy — which
    /// the frontend does not act on today but could log).
    pub const WORKSPACE_ACTIVATE: &str = "workspace.activate";
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
    /// `AgentSendRes{ok, receivers}` ack. Structurally mirrors `WORKSPACE_CHANGED`
    /// but adds the client→daemon publish leg.
    pub const AGENT_SEND: &str = "agent.send";
    /// Server→client push fired for every relayed agent message (see
    /// `AGENT_SEND`). Mirrors `workspace.changed`: the daemon broadcasts to
    /// every connection so the in-terminal agent on the other machine receives
    /// the message instantly instead of polling the slow git bus. Payload
    /// carries `{from, to, text, ts}`; the daemon whose comm folder lists `to`
    /// files it (`hub_link.rs`) and answers `agent.filed`.
    pub const AGENT_MESSAGE: &str = "agent.message";
    /// Client→daemon request (ADR 0048): whoever APPENDED a relayed frame
    /// to an inbox says so. Payload `AgentFiledReq { id }` — the
    /// sender-minted id the frame carried, and nothing else. The daemon
    /// stamps `filer` from THIS connection's declared hello `name`, fans
    /// the result out as an `AGENT_RECEIPT` evt, and answers
    /// `AgentFiledRes { ok }`. A connection with no declared name has
    /// nothing to be named as and is refused `bad_filer`. The daemon keeps
    /// no delivery state: it relays a receipt exactly as it relays a
    /// message.
    ///
    /// **Only a POSITIVE claim exists.** A filer knows it appended; it
    /// cannot know that no OTHER filer did, and `agent.message` reaches
    /// every connection, so a "did not file" answer would be a global
    /// assertion made from local knowledge — every attached frontend would
    /// deny a handle it does not host, and whichever denial arrived first
    /// would overrule the truth. Absence of a receipt is the only negative,
    /// and it is the sender's own conclusion, not anyone's claim.
    pub const AGENT_FILED: &str = "agent.filed";
    /// Server→client push fired for every `AGENT_FILED` (ADR 0048), to
    /// every connection like `AGENT_MESSAGE` — the sender recognizes its
    /// own by `id` and ignores the rest. Payload `AgentReceiptEvt { id,
    /// filer }`. This is the only honest verdict for a cross-box send: the
    /// append IS the delivery, so only the appender can report one.
    ///
    /// `filer` is ATTRIBUTION, not authentication: nothing validates a
    /// hello `name`, and the frame id reaches every client, so any
    /// authenticated client could vouch under any name. It replaces a
    /// name-suffix guess with a claim from something that says it did the
    /// work, checkable against the roster — not a proof against a hostile
    /// client, which is not the threat model here.
    pub const AGENT_RECEIPT: &str = "agent.receipt";
    /// Client→daemon request (0031 B1): append this frame to `to`'s inbox in
    /// THIS daemon's comm folder — the hub files for its own home, and a
    /// guest on the hub's folder forwards what it cannot prove it may append.
    /// Payload `CommFileReq { from, to, text, broadcast, forwarded }`;
    /// response `CommFileRes { ok: true }` only when the line is in the file,
    /// else the standard `{error, code}` with `code` one of `bad_handle`,
    /// `not_here` (this folder does not list `to`), `no_live_session`,
    /// `file_failed` — and nothing was appended.
    /// One verb per job: `AGENT_SEND` still means "publish onto the bus".
    pub const COMM_FILE: &str = "comm.file";
    /// Client→daemon request: a session inside a workspace declares its
    /// sot-comm handle to the daemon that spawned/pinned its env (ADR
    /// 0046 decision 1), over the typed owner endpoint (`SOT_SOCKET`) —
    /// never the relay. Payload `AgentJoinReq { workspace_id, handle }`.
    /// The daemon persists `Workspace.agent_handle`, publishes
    /// `workspace.changed`, and answers `AgentJoinRes { ok }`; refusals
    /// (`unknown_workspace`, `bad_handle`) ride the standard error
    /// payload. Replaces the daemon's own self-file read-back
    /// (`capsule_comm_handle`) — the session declares once instead of the
    /// daemon re-deriving it from disk on every read.
    pub const AGENT_JOIN: &str = "agent.join";
    /// Client→daemon request: drive the frontend(s) with an imperative UI command
    /// (ADR 0025). Mirrors `AGENT_SEND`'s publish leg — the daemon re-emits the
    /// body as an `FE_COMMAND` evt to connected FEs. Unlike the relay `nav.preview`
    /// envelope (gated, workspace-scoped), this is *imperative*: the FE switches +
    /// shows regardless of its current view, under a badge-floor + opt-in
    /// force-show consent model (FE-side). Payload `FeCommandSendReq`
    /// (`{cmd, args, target?}`); response
    /// `FeCommandSendRes{ok, resolved_target?, delivered_to?}`.
    /// Sent by the `sot-fe` BE CLI over the daemon socket — no comm relay, no FE LLM.
    pub const FE_COMMAND_SEND: &str = "fe.command.send";
    /// Server→client push carrying one imperative FE command (ADR 0025). Mirrors
    /// `AGENT_MESSAGE`: the daemon broadcasts to every connection; the FE parses
    /// the `{v, cmd, args}` envelope into an `FeCommand` and runs it through the
    /// existing `dispatch_fe_command` sink. `target` (a FE sot-comm handle)
    /// optionally scopes it: `None` → every FE acts (the badge floor);
    /// `Some(handle)` → only the matching FE acts (force-show to a specific FE).
    /// An untargeted `fe.command.send` is auto-resolved daemon-side to the
    /// active frontend (2026-09-08 review rework) — exclusively, by connection
    /// identity, not merely by re-checking `target` FE-side (see
    /// `FeCommandEvt::target_serial`). Payload `FeCommandEvt`.
    pub const FE_COMMAND: &str = "fe.command";
    /// A person is providing real input right now (2026-09-08 review rework,
    /// design point A). Sent by the frontend from its own winit
    /// keyboard/mouse handlers, throttled client-side — never by any
    /// automated or command-file-driven path. Empty request
    /// (`FePresenceReq`); the daemon stamps this connection's
    /// `last_person_input_at` and acks (`FePresenceRes{ok}`). This is the
    /// ONLY thing that stamps it — replaces an earlier design that inferred
    /// presence from ordinary navigation/typing ops, which turned out to
    /// have automated producers for every one of them.
    pub const FE_PRESENCE: &str = "fe.presence";
    /// Client→daemon: a frontend declares the sot-comm handles its own
    /// box's daemon owns, so THIS daemon (which never sees that box's
    /// rows directly) can list them (session-listing brief). Payload
    /// `FeSessionsReq { sessions }`; response `FeSessionsRes { ok: true }`
    /// always — the same "never fails" shape as `fe.presence`, since there
    /// is nothing here that can be refused. Re-sent whenever the sending
    /// box's own row list OR any row's state changes (edge-driven, no
    /// timer) — see `Clients::declare_sessions`.
    pub const FE_SESSIONS: &str = "fe.sessions";
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
    /// Apply the ARMED pending update now (ADR 0030 Phase C3). Empty request
    /// (`UpdateApplyReq`). The daemon validates that a pending pointer is
    /// armed, answers `UpdateApplyRes { ok, tag, will_restart, status }`,
    /// broadcasts a notify, then EXITS — the single apply owner (systemd
    /// ExecStartPre, or the next `sot-launch`) performs the fast offline
    /// flip and brings up the new version. `will_restart` is true only under
    /// a service manager; otherwise the user's next launch completes it.
    pub const UPDATE_APPLY: &str = "update.apply";
    /// TCP proxy handshake (ADR 0035). Sent as the FIRST frame on a
    /// DEDICATED daemon-socket connection (never on the multiplexed control
    /// connection): `ProxyConnectReq { port, token? }`. The daemon validates
    /// the port against its served-port allowlist, dials
    /// `127.0.0.1:<port>`, answers `ProxyConnectRes { ok: true }`, and from
    /// that moment the connection is a raw byte pipe
    /// (`copy_bidirectional`) until either side closes — which is what
    /// carries WebSocket upgrades (Bonito/WGLMakie) unmodified. Rejections
    /// ride the standard error payload (`bad_port`, `dial_failed`) and the
    /// connection closes.
    pub const PROXY_CONNECT: &str = "proxy.connect";
    /// A capsule row's supervisor or voyage lane, piped through the row's
    /// OWN daemon (ADR 0045 decision 2). Sent as the FIRST frame on a
    /// DEDICATED connection — the `proxy.connect` peek, never inside the
    /// hello-gated control loop: `LaneConnectReq { target, lane,
    /// voyage_id?, token? }`. The daemon resolves `target` (the row's
    /// `session_name`, as `pty.open` addresses it) to a capsule
    /// workspace, dials that row's lane locally, resumes an absent
    /// supervisor lane in place (`resume_if_absent`, never a voyage
    /// lane), authenticates the peer it dialed, and answers
    /// `LaneConnectRes { ok: true, pid, created }` before becoming a raw
    /// byte pipe — the daemon never decodes a lane frame after the
    /// reply. Rejections (`bad_request`, `unauthenticated`,
    /// `unknown_workspace`, `not_capsule`, `bad_lane`, `lane_absent`,
    /// `foreign`, `undetermined`, `dial_failed`) ride the standard error
    /// payload and the connection closes.
    pub const LANE_CONNECT: &str = "lane.connect";
    /// Every runtime answers what build it is (ADR 0030 §8 decision 31,
    /// cross-referenced as ADR 0043 decision 31). Empty request
    /// (`VersionQueryReq`); pure in-memory, no fan-out, no supervisor probe
    /// — `workspace.list` already probes every capsule row per call, so
    /// this never duplicates that. Answers `VersionQueryRes { daemon,
    /// clients }`: this daemon's own version triple, plus one entry per
    /// currently-attached frontend (sourced from the hello each already
    /// sent). An old daemon that predates this op answers the generic
    /// unknown-op payload (`{"error": "unknown op: version.query"}` on a
    /// `res` frame carrying this same op) — callers must treat that as
    /// "daemon predates this op", not a failure.
    pub const VERSION_QUERY: &str = "version.query";
    /// Topology plan §F step 2 (the half-open-roster defect): a bare
    /// liveness probe for the two long-lived roles, `fe` and `bridge`.
    /// Empty request (`PingReq`); the daemon answers `PingRes{ok:true}`
    /// with no side effect beyond proving the read half of this
    /// connection is still alive. Sent every `PING_INTERVAL` by the
    /// frontend transport;
    /// the daemon gives an `fe`/`bridge` connection a `READ_DEADLINE`
    /// (server.rs) and drops one that goes quiet that long, reaping it
    /// through the same `ClientGuard::drop` path as a clean exit.
    /// `cli`/`agent` (one-shot) connections never send this and are never
    /// deadline-gated. Opt-in by ping (manager compatibility fix): the
    /// deadline arms on this connection's FIRST `ping`, never at hello —
    /// an `fe`/`bridge` peer too old to send one (a frontend box or comm
    /// bridge that hasn't converged from main yet) is left untouched,
    /// exactly today's behavior, rather than reaped every `READ_DEADLINE`
    /// forever. An old daemon that predates this op answers the generic
    /// unknown-op payload, same shape as a legacy `version.query` caller
    /// sees — a sender must tolerate that (it means "daemon predates this
    /// op", not a failure) rather than treating it as proof the peer is
    /// dead.
    pub const PING: &str = "ping";
    /// Topology plan §B "Editing the master list": edit the declared
    /// topology (`hosts.toml`, grammar v2). Payload `TopologySetReq {
    /// edit: topology::TopologyEdit }` (add/remove a host, flip one of its
    /// flags, add/remove a `[monitor]` label). Authorisation is the dial
    /// itself — whoever can reach this daemon's socket may send this op;
    /// no second credential (the 0700 socket and ssh identity already gate
    /// who can dial at all). Only the daemon declared as `hub` in its OWN
    /// currently-loaded file applies an edit; every other daemon refuses,
    /// naming the hub (its file is a `topology sync` CACHE, never
    /// writable). The hub daemon re-reads its file from disk (picking up
    /// any hand edit since the last op — same on-demand-refresh path
    /// `version.query` uses), applies the edit, validates the result by
    /// re-parsing it with [`crate::topology::parse`] (the one grammar —
    /// this is what catches "cleared `daemon` on the hub" and similar
    /// structural nonsense generically), and writes tmp+rename. Refused
    /// (standard error payload, `code` named): `not_hub`; removing the
    /// hub; removing/un-daemon-ing the host the REQUEST'S OWN `hello`
    /// declared (`HelloReq.host`) — a box cannot edit itself out from
    /// under its own connection; removing or clearing `daemon` on a host
    /// with running capsule rows — checked against the HUB's OWN rows
    /// only (a star: the hub cannot see another daemon's rows, so a
    /// non-hub box's `sotd topology set` CLI does that check locally,
    /// against ITS OWN daemon, before ever dialing the hub); `invalid`
    /// (the structural [`crate::topology::apply`] step or the final
    /// re-parse rejected it). On success, answers `TopologySetRes { ok:
    /// true, hash }` and broadcasts [`TOPOLOGY_CHANGED`] to every attached
    /// client — the SAME broadcast a picked-up hand edit fires, so both
    /// routes are one code path.
    pub const TOPOLOGY_SET: &str = "topology.set";
    /// Server→client push fired once per successful topology write —
    /// either a `topology.set` this daemon applied, or a hand edit on disk
    /// this daemon noticed on its next on-demand re-read (never a file
    /// watcher: the plan is explicit that `notify` misses writes on a
    /// network filesystem). Mirrors `WORKSPACE_CHANGED`: broadcast to
    /// every connection so Hosts mode refreshes live instead of polling.
    /// Payload is `{"hash": "<hex>"}` — the new file's `hash_text`; a
    /// receiver that cares about WHAT changed re-issues `version.query` or
    /// `topology sync` rather than diffing a delta this event doesn't
    /// carry.
    pub const TOPOLOGY_CHANGED: &str = "topology.changed";

    /// First frame of a dedicated lease connection: the window claims this
    /// daemon. Payload [`crate::ops::FeLeaseReq`]; reply [`crate::ops::FeLeaseRes`].
    pub const FE_LEASE: &str = "fe.lease";
    /// Only on a granted lease connection: the window says how it is
    /// leaving. Payload [`crate::ops::FeLeavingReq`]; reply [`crate::ops::FeLeavingRes`].
    pub const FE_LEAVING: &str = "fe.leaving";
    /// Only on a granted lease connection: the window has shown the
    /// not-ended notice. Payload [`crate::ops::FeNoticeSeenReq`]; reply
    /// [`crate::ops::FeNoticeSeenRes`].
    pub const FE_NOTICE_SEEN: &str = "fe.notice_seen";
}

mod session;
pub use session::*;

mod browse;
pub use browse::*;

mod repl;
pub use repl::*;

mod pty;
pub use pty::*;

mod workspace;
pub use workspace::*;

mod agent;
pub use agent::*;

mod pages;
pub use pages::*;

mod monitor;
pub use monitor::*;

mod update;
pub use update::*;

mod lane;
pub use lane::*;

pub mod lease;
pub use lease::{FeLeaseReq, FeLeaseRes, FeLeavingReq, FeLeavingRes, FeNoticeSeenReq, FeNoticeSeenRes, LeaseOutcome, LeaveIntent};
