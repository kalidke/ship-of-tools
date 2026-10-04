// transport.rs — local frontend ↔ remote backend over a local socket (Unix
// socket / Windows named pipe) or an ssh child's stdio.
//
// Per ADR 0010, as replaced by isolation-plan.md §3 C3 (amended by
// dev/output/c3-second-connection-amendment.md):
//   - Backend listens on a per-session Unix socket on the remote
//     ($XDG_RUNTIME_DIR/sot/<session_id>.sock), inside a tmux session.
//   - Reaching a daemon that is not on this box means spawning
//     `ssh <target> '<PATH prelude>; sotd stdio-bridge [--host <host>]'`
//     (`sot_protocol::ssh_bridge`) and speaking the protocol over its piped
//     stdin/stdout — never a port, on either box: the daemon has had no TCP
//     listener since 0.4.0. A dead login or a dead `sotd` on the far end is
//     the child exiting before the first frame; its last stderr line IS the
//     diagnosis (no per-cause exit codes to invent).
//   - Connect handshake carries (session_id, client_id, last_seen_revision);
//     backend either replays missed events or sends a snapshot on reconnect.
//
// Transport selection: `spawn` takes a pipe or an ssh recipe. The protocol
// code is generic over `AsyncRead` / `AsyncWrite` so it runs identically on
// either transport — an ssh child is a third `AsyncRead`/`AsyncWrite` pair,
// not a protocol change (the amendment's own §0).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;
use std::sync::Arc;

use crate::dial::HostKey;
use anyhow::{Context, Result};
use base64::Engine;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath,
};
use serde_json::Value;
use sot_protocol::{
    codec, op, AgentSendReq, ConceptReadReq, ConceptReadRes, ConceptWriteReq, ConceptWriteRes,
    DirCreateReq, DirCreateRes, DocsOpenReq, DocsOpenRes, FePresenceReq, FileChunk, FileDeleteReq, FileDeleteRes,
    FileDownloadReq, FileReadReq, FileReadRes, FileUploadAck, FileUploadReq, FileWriteReq,
    FileWriteRes, Frame,
    HelloReq, HelloRes, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    MonitorHistoryReq, MonitorHistoryRes, MonitorSubscribeRes, MonitorTickEvt, PingReq, PlutoOpenReq,
    PlutoOpenRes, PreviewGetReq, PreviewGetRes, PtyOpenReq,
    QuartoOpenReq, ReplEvalReq, ReplEvalRes, ReplFrame, ReplFrameEvt, ReplRunFileReq,
    ReplRunFileRes, ToggleHiddenReq, TreeChildrenReq, TreeChildrenRes, TreeNode, TreeRootReq,
    TreeRootRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceListReq,
    WorkspaceListRes,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};

use winit::window::Window;

/// What the transport task should dial: a local socket/named pipe, or an
/// ssh child's stdio (C3). Exactly one, never neither and never both —
/// replaces the former `pipe: Option<PathBuf>` / `ssh: Option<SshRecipe>`
/// pair, whose "at least one must be set" was a doc caveat callers had to
/// honor by convention (the CLI never actually built one with neither set;
/// the only construction with both was a `#[cfg(test)]` state the CLI
/// cannot produce) rather than a fact the type itself enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    Pipe(PathBuf),
    Ssh(sot_protocol::ssh_bridge::SshRecipe),
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub dial: Dial,
    pub token: Option<String>,
}

/// ADR 0045 decision 1 (Codex review, lane B5 discharge); reshaped by C3 as
/// amended: which transport a host's CONTROL connection actually resolved
/// to — `Local` (the pipe/socket connected) or `Ssh` (the ssh child
/// connected, carrying the exact recipe it spawned). Recorded from every
/// `Connected` evt (`State::host_resolved_dial`) so `spawn_pane_attach_term`
/// dials the SAME endpoint the control connection is already talking to,
/// rather than an independent preference guess that could reach a
/// DIFFERENT daemon than the one actually running this host.
///
/// Lives here, beside `TransportConfig`, because `IncomingEvt::Connected`
/// carries it — moved out of `gpu.rs`, which names it
/// `crate::transport::ResolvedDial`. No longer `Copy` (`SshRecipe` isn't):
/// every former `.copied()` reader became `.cloned()` (the amendment's own
/// site list, C3's commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedDial {
    Local,
    Ssh(sot_protocol::ssh_bridge::SshRecipe),
}

mod event;
pub(crate) use event::IncomingEvt;

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
    pub session_name: String,
    pub kernel_running: bool,
    pub is_default: bool,
    /// Which agent this workspace auto-starts: "claude" | "codex" | "none".
    /// A host's DEFAULT row with `agent == "none"` is the inert anchor (ADR
    /// 0042 amendments 2026-09-04 / 2026-09-06), on every runtime —
    /// `session_host_children` filters it out of the Sessions tree entirely
    /// rather than rendering it as a session.
    pub agent: String,
    /// Contract (b): the FE launches claude (ccb) on first attach to a
    /// workspace with `autostart_claude == true`. `agent_name` is the comm
    /// handle the bootstrap joins as (informational on the FE side).
    pub autostart_claude: bool,
    pub agent_name: String,
    /// The sot-comm handle the session inside this workspace actually
    /// declared via `agent.join` — the **joined** handle, mirroring the
    /// wire's `WorkspaceListEntry.agent_handle`. Distinct from
    /// `agent_name` above, which is only what the workspace was CREATED
    /// to expect: the two can differ, and only this one is what a sender
    /// actually addresses. `WorkspaceInfo` doesn't derive
    /// Serialize/Deserialize (it's constructed by hand from the wire
    /// type at the one parse site below), so there's no `#[serde(default)]`
    /// to mirror here — empty string = never joined, same as the wire
    /// field's own empty-string-means-absent convention.
    pub agent_handle: String,
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
    /// Lifecycle of the workspace's persistent REPL child: "not_started" |
    /// "starting" (spawned, precompiling — NOT dead) | "ready" | "dead";
    /// "" from a daemon that predates the field. Mirrors the `lifecycle`
    /// repl.frame evt for FEs that (re)connect mid-boot.
    pub repl_state: String,
    /// ADR 0042 slice L1a/L1b: `"tmux"` | `"capsule"` — which runtime
    /// hosts this workspace's agent pane. `""` from a daemon that
    /// predates L1a. ADR 0042 shrink round (rule A): no longer consulted
    /// by the attach path at all — `attach_session_to_bl` always sends
    /// `pty.open` and lets the daemon's own `attach_direct` reply decide
    /// (the only kind this build's daemon sends), so an old daemon (or
    /// one reporting `""`) changes nothing there either.
    ///
    /// Deserialized on every platform (the wire contract doesn't fork by
    /// FE OS). Historically read by gpu.rs only from `#[cfg(windows)]`
    /// call sites (L1b fix 5: capsule has nothing to attach to off
    /// Windows) — 2026-09-04 amendment adds one platform-agnostic
    /// reader, `session_host_children`'s inert-anchor filter, which must
    /// tell a Windows capsule default row (`agent == "none"` there means
    /// "never seeded an agent") apart from a shared backend's own TMUX
    /// default row (`agent == "none"` there is normal — the SoT LLM
    /// lives in the drawer). `""` from a daemon that predates L1a reads
    /// as neither and is simply never filtered.
    pub runtime: String,
    /// The supervisor-lane phase (ADR 0041 Lifecycle, snake_case),
    /// `"stopped"` (its state directory was never created — no supervisor
    /// has ever run for it), or `"unreachable"` (the lane could not be
    /// queried at all) — `Some` only for `runtime == "capsule"` rows.
    /// Folded into the Sessions row's glance line (`capsule_phase_tag`).
    pub phase: Option<String>,
    /// Per-session accounts (owner-simplified brief, 2026-09-15): the
    /// login directory this row's agent runs under. `""` = the agent's
    /// default directory — the common case, and the only value from a
    /// daemon that predates the field.
    pub account: String,
}

impl WorkspaceInfo {
    /// The daemon's default row in its inert-anchor state: home-rooted, no
    /// agent, not a session -- on EVERY host (owner ruling 2026-09-06: a row
    /// that looks like a session but cannot be closed, and invites an LLM
    /// pane it must not have, confuses; the SoT LLM lives in the drawer, and
    /// Ship of Tools development runs in its own workspace row like any
    /// other project). Shared by the Sessions tree (`session_host_children`)
    /// and the bottom strip's cache build (`fresh_workspace_caches`) so the
    /// two never drift on what "inert" means.
    pub(crate) fn is_inert_anchor(&self) -> bool {
        self.is_default && self.agent == "none"
    }
}

/// One row of the `accounts.list` response (owner-simplified brief,
/// 2026-09-15). Mirrors `sot_protocol::AccountEntry`. `"default"` sorts
/// first.
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub name: String,
    pub kinds: Vec<String>,
    pub logged_in: std::collections::HashMap<String, bool>,
}

impl AccountInfo {
    /// True if none of this account's declared kinds have a login here —
    /// the new-session prompt dims the row and appends "(not logged in)",
    /// still selectable: the daemon refuses with the exact fix command.
    pub fn any_logged_in(&self) -> bool {
        self.kinds.iter().any(|k| self.logged_in.get(k).copied().unwrap_or(false))
    }
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
    pub session_name: String,
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
    /// `Some(detail)` when the backend kept the row instead of removing
    /// it (the default workspace's capsule run was ended in place — see
    /// `sot_protocol::ops::WorkspaceDestroyRes::kept`); `None` for an
    /// ordinary destroy where the row is actually gone.
    pub kept: Option<String>,
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

/// Outcome of a `dir.create` request, mirroring the backend's two response
/// shapes (success / error, incl. `code: "already_exists"`).
#[derive(Debug, Clone)]
pub enum DirCreateResult {
    /// Directory created; `path` is the absolute path on disk.
    #[allow(dead_code)]
    Ok { path: String },
    /// Any backend error — `code` is the protocol code (`bad_node_id`,
    /// `already_exists`, `dir_create_failed`, …), `message` detail.
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

mod request;
pub(crate) use request::OutgoingReq;
use request::send_request;

mod ops;
use ops::*;

mod reply;
use reply::{handle_response_frame, PendingGuard, PendingKind};


/// Create the outgoing-request channel paired with the transport task. The
/// sender lives on the GPU thread; the receiver gets handed to `spawn`. Both
/// sides drop their handle on shutdown — that's how the writer half of the
/// select loop terminates.
pub fn outgoing_channel() -> (UnboundedSender<OutgoingReq>, UnboundedReceiver<OutgoingReq>) {
    tmpsc::unbounded_channel()
}

/// Next reconnect wait after a failed attempt: double, up to a cap that
/// depends on the dial. A local socket costs nothing to probe, so it keeps
/// 5 s. Each ssh probe is a login on the hub plus one on the far host, so
/// the cap is 30 s: once the wait reaches it, a down remote host costs the hub
/// at most two ssh logins a minute per frontend (about nine in the first minute,
/// while the wait doubles).
fn next_backoff_ms(current: u64, dial: &Dial) -> u64 {
    let cap = match dial {
        Dial::Pipe(_) => 5_000,
        Dial::Ssh(_) => 30_000,
    };
    current.saturating_mul(2).min(cap)
}

/// Spawn the transport task on `rt`. Returns once spawned; the task runs
/// until the connection drops or the runtime shuts down. The task asks the
/// window to redraw whenever a new IncomingEvt is published so the GPU
/// thread sees state updates without polling.
pub fn spawn(
    rt: &tokio::runtime::Runtime,
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    reconnect_now: Arc<tokio::sync::Notify>,
    gate: sot_protocol::ssh_bridge::LinkGate,
    leases: Arc<crate::lease::Leases>,
) {
    rt.spawn(async move {
        // Reconnect loop with exponential backoff, capped at 5s on a local
        // socket and 30s on an ssh dial (each ssh probe is a login on the
        // hub and another on the far host). The
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
        loop {
            match connect_and_run(
                host.clone(),
                config.clone(),
                evt_tx.clone(),
                &mut out_rx,
                window.clone(),
                &mut backoff_ms,
                &gate,
                &leases,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!(%host, "transport task exited cleanly");
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        %host,
                        error = %format_args!("{e:#}"),
                        backoff_ms,
                        "transport task ended; reconnecting"
                    );
                    let _ = evt_tx.send((
                        host.clone(),
                        IncomingEvt::Disconnected {
                            reason: format!("{e:#} — retry in {backoff_ms}ms (F5 to retry now)"),
                        },
                    ));
                    window.request_redraw();
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                        _ = reconnect_now.notified() => {
                            tracing::info!("manual reconnect requested — collapsing backoff");
                            backoff_ms = BACKOFF_FLOOR_MS;
                            continue;
                        }
                    }
                    backoff_ms = next_backoff_ms(backoff_ms, &config.dial);
                }
            }
        }
    });
}

/// Dial whichever transport `config.dial` names. Once a connection is
/// established we hand off to `run_protocol`; any error from there is
/// *not* retried via the other transport — that's a runtime disconnect,
/// not a startup-time choose-your-transport decision. There is no
/// fallback between the two arms: pre-C3 both could be configured and a
/// pipe failure fell through to ssh, but `Dial` makes that unconstructible
/// now, so each arm either connects and hands off or returns its own
/// `Err`.
async fn connect_and_run(
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    backoff_ms: &mut u64,
    gate: &sot_protocol::ssh_bridge::LinkGate,
    leases: &crate::lease::Leases,
) -> Result<()> {
    match &config.dial {
        Dial::Pipe(pipe_path) => {
            let not_ended = leases
                .before_data_connection(&host, pipe_path, config.token.as_deref())
                .await?;
            // The grant recorded the count (`Leases::owed`); the next frame shows it.
            if not_ended > 0 {
                window.request_redraw();
            }
            let stream = connect_pipe(pipe_path).await?;
            // Pre-hello: the daemon hasn't declared its host yet, so
            // `host` here is only this connection's DIAL key, not a
            // claim about identity (ADR 0046 decision 1) — label it
            // plainly so it's never misread as the declared value the
            // later `"connected"` line's `declared` field carries.
            tracing::info!(dial = %host, ?pipe_path, "connected via local socket");
            let (rx, tx) = stream.split();
            let rx = codec::buffered(rx);
            run_protocol(
                host,
                rx,
                tx,
                config.token.as_deref(),
                &evt_tx,
                out_rx,
                &window,
                backoff_ms,
                ResolvedDial::Local,
                None,
            )
            .await
        }
        Dial::Ssh(recipe) => {
            let mut child = sot_protocol::ssh_bridge::LinkGate::probe(recipe)
                .with_context(|| format!("spawn ssh {recipe}"))?;
            let stdin = child.stdin.take().expect("spawned with a piped stdin");
            let stdout = child.stdout.take().expect("spawned with a piped stdout");
            let stderr = child.stderr.take().expect("spawned with a piped stderr");
            // The child's last non-empty stderr line, drained on its own task
            // for as long as `child` lives — ssh's own complaint ("Permission
            // denied", or `unrecognised argument: --host` from a hub whose
            // `sotd` predates C1) is the diagnosis a dead child leaves behind,
            // the same rule `stdio_bridge.rs` already sets for the far end.
            let last_stderr = Arc::new(std::sync::Mutex::new(None::<String>));
            {
                let last_stderr = Arc::clone(&last_stderr);
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = tokio::io::BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if !line.trim().is_empty() {
                            if let Ok(mut guard) = last_stderr.lock() {
                                *guard = Some(line);
                            }
                        }
                    }
                });
            }
            // Pre-hello, same labeling rule as the pipe branch above.
            tracing::info!(dial = %host, %recipe, "connected via ssh child");
            let rx = codec::buffered(stdout);
            // The gate goes down inside `run_protocol`'s own wrapper, before
            // the stderr wait below, so no lane dials while that wait runs.
            let result = run_protocol(
                host,
                rx,
                stdin,
                config.token.as_deref(),
                &evt_tx,
                out_rx,
                &window,
                backoff_ms,
                ResolvedDial::Ssh(recipe.clone()),
                Some(gate),
            )
            .await;
            // `child` is dropped when this arm returns (`kill_on_drop`), after the
            // stderr read below, ending the ssh login this attempt owns before the
            // reconnect loop's next attempt spawns a fresh one.
            if let Err(e) = &result {
                if let Some(line) = sot_protocol::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
                    return Err(anyhow::anyhow!("{e:#} (ssh: {line})"));
                }
            }
            result
        }
    }
}

/// Connect to the local socket / named pipe at `path`.
pub(crate) async fn connect_pipe(path: &std::path::Path) -> Result<LocalStream> {
    let path_str = path.to_str().context("socket path must be valid UTF-8")?;
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    LocalStream::connect(name)
        .await
        .with_context(|| format!("connect {path:?}"))
}

use crate::net::state::{note_revision, SessionState, StateSaveGate};

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

/// How often this connection sends `ping` (topology plan §F step 2) — a
/// third of the daemon's own `PING_READ_DEADLINE` (90s, `server.rs`), so a
/// missed tick or two is noise and three in a row is what actually trips
/// the daemon's reaper. `SOT_TEST_PING_INTERVAL_MS` overrides it for tests
/// (same `OnceLock`-cached-once-per-process convention the backend uses
/// for its own deadline override); unset in every real deployment.
fn ping_interval_duration() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_PING_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(30))
}

mod hello;
use hello::{accept_hello, read_hello, send_hello, HelloRefused};

mod preamble;
use preamble::{preamble_preview, preamble_tree_root};

/// What `run_protocol` needs of the window: a redraw request. A trait so a
/// test can run the protocol without a real window.
trait Redraw {
    fn request_redraw(&self);
}

impl Redraw for Arc<Window> {
    fn request_redraw(&self) {
        Window::request_redraw(self);
    }
}


/// Run the session and write the link gate: up at any hello reply (inside
/// [`run_session`]), down when the session ends for any reason except a
/// refusal of the hello itself. `gate` is `None` for a local connection,
/// which has no ssh link to gate.
async fn run_protocol<R, W, Wn>(
    host: HostKey,
    rx: tokio::io::BufReader<R>,
    tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    backoff_ms: &mut u64,
    resolved: ResolvedDial,
    gate: Option<&sot_protocol::ssh_bridge::LinkGate>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let result = run_session(host, rx, tx, token, evt_tx, out_rx, window, backoff_ms, resolved, gate).await;
    if let Some(gate) = gate {
        if !matches!(&result, Err(e) if e.is::<HelloRefused>()) {
            gate.set_up(false);
        }
    }
    result
}

/// Drive the wire protocol over an already-connected stream's halves. Generic
/// over the read/write types so the same code path serves the local-socket
/// transport and an ssh child's stdio — C3 as amended §0: adding this
/// transport inside the protocol CRATE is not a wire-protocol change; the
/// frames this function reads/writes are untouched.
async fn run_session<R, W, Wn>(
    host: HostKey,
    mut rx: tokio::io::BufReader<R>,
    mut tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    backoff_ms: &mut u64,
    // C3 as amended §5: which transport `connect_and_run` actually
    // connected — `ResolvedDial::Local` for the pipe, `ResolvedDial::Ssh`
    // for the ssh child, carrying the exact recipe it spawned. The proxy
    // arms only when NOT `Local` — keyed on the transport that CONNECTED,
    // not the CLI shape.
    resolved: ResolvedDial,
    gate: Option<&sot_protocol::ssh_bridge::LinkGate>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let mut next_id: u64 = 1;
    // PendingGuard, not a bare HashMap: its Drop flushes any surviving
    // FigureGet entries as FigureGetFailed on every exit path this
    // function has (see the type's doc comment) — the fix for a
    // connection dropping between a figure.get and its reply.
    let mut pending = PendingGuard {
        map: HashMap::new(),
        evt_tx,
        host: host.clone(),
    };
    // Tags every run_protocol-level send with `host` — ADR 0042 L2a: the
    // app's receiver is `Receiver<(HostKey, IncomingEvt)>`, tagged at each
    // send (this closure), not by a separate forwarding task.
    // `handle_response_frame` (a separate fn, most of the actual sends)
    // carries its own copy of the same pattern.
    let emit = {
        let host = host.clone();
        move |ev: IncomingEvt| {
            let _ = evt_tx.send((host.clone(), ev));
        }
    };

    // Reconnect memory: client_id stays stable across runs; session_id +
    // last_seen_revision feed the backend's replay path. First-ever launch
    // produces fresh values and the backend assigns a session_id we'll
    // remember for next time.
    // `session` bundles the memory with its `StateSaveGate` (see
    // `StateSaveGate`'s doc: throttles `crate::state::save` so a burst of
    // `rev`-bearing replies can't stall this task's read future on disk
    // I/O) and, via its `Drop`, flushes whatever the throttle held back the
    // moment this connection ends — see `SessionState`.
    let mut session = SessionState {
        host: host.clone(),
        memory: crate::state::load(&host),
        gate: StateSaveGate::new(),
    };
    tracing::info!(
        %host,
        client_id = %session.memory.client_id,
        ?session.memory.session_id,
        last_seen_revision = session.memory.last_seen_revision,
        token_set = token.is_some(),
        "loaded session memory"
    );

    // hello
    let hello_id = take_id(&mut next_id);
    send_hello(&mut tx, hello_id, &session, token).await?;
    let frame = read_hello(&mut rx, hello_id, gate, &mut session, &emit, window).await?;
    accept_hello(frame, &host, &mut session, backoff_ms, resolved, &emit, window)?;

    // tree.root — initial fetch on connect uses the default workspace
    // (no workspace_id). Once the chrome resumes a saved Sessions-mode
    // active_workspace_id it will re-fire this with the id set.
    let tree_id = take_id(&mut next_id);
    let root_node_id = preamble_tree_root(&mut tx, &mut rx, tree_id, &host, &mut session, &emit, window).await?;

    // preview.get against whatever the backend just reported as the root.
    // For the spike that's enough to prove blob round-trip; real previews
    // follow user navigation.
    let prev_id = take_id(&mut next_id);
    preamble_preview(&mut tx, &mut rx, prev_id, root_node_id, &host, &mut session, &emit, window).await?;

    steady_loop(rx, &mut tx, next_id, &mut pending, &mut session, host, evt_tx, out_rx, window).await
}

/// The steady-state loop: replies and pushed events, the ping, and the window's requests.
async fn steady_loop<R, W, Wn>(
    rx: tokio::io::BufReader<R>,
    mut tx: W,
    mut next_id: u64,
    pending: &mut HashMap<u64, PendingKind>,
    session: &mut SessionState,
    host: HostKey,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
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
    // Topology plan §F step 2 (the half-open-roster fix): this connection
    // is always `fe`-declared (see `hello` above), one of the daemon's two
    // long-lived roles, so it always pings — no role check needed here,
    // unlike the daemon side which also has to let `cli`/`agent` through
    // ungated. `Interval`, not a plain `sleep_until` recomputed each loop:
    // it owns its own next-tick state and its `tick()` is cancellation-
    // safe, so a `select!` iteration that takes another arm just leaves it
    // armed for next time instead of losing the schedule.
    let mut ping_interval = tokio::time::interval(ping_interval_duration());
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await; // first tick fires immediately; consume it
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
                note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                handle_response_frame(frame, blob, pending, evt_tx, &host);
                window.request_redraw();
            }

            // Topology plan §F step 2: prove this connection's read half is
            // alive to the daemon even when the person is idle (no other
            // outgoing traffic). Fire-and-forget, same idiom as
            // `OutgoingReq::FePresence` below — no `PendingKind`, the reply
            // is silently ignored by the unmatched-id fallthrough.
            _ = ping_interval.tick() => {
                let id = take_id(&mut next_id);
                tracing::debug!(id, "→ ping");
                codec::write_frame(
                    &mut tx,
                    &Frame::req(id, op::PING, serde_json::to_value(PingReq {})?),
                    None,
                )
                .await?;
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
                    note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                    handle_response_frame(frame, blob, pending, evt_tx, &host);
                    window.request_redraw();
                    loop {
                        let (frame, blob) = codec::read_frame(&mut rx).await?;
                        note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                        handle_response_frame(frame, blob, pending, evt_tx, &host);
                        window.request_redraw();
                    }
                };
                let id = take_id(&mut next_id);
                send_request(&mut tx, pending, id, req).await?;
            }
        }
    }
}

/// ADR 0042 slice L1b, revised by ADR 0045 decision 1: is `payload` a
/// `pty.open` refusal carrying `code: "attach_direct"` (the daemon's
/// answer for a capsule-runtime workspace, `rust/backend/src/server.rs`'s
/// `PTY_OPEN` arm)? The daemon still emits a `state_dir` alongside it
/// (until the next `PROTOCOL_VERSION` bump) but the frontend no longer
/// reads it — every capsule row is attached through its own daemon's
/// `lane.connect` bridge, keyed by `target` alone.
fn is_attach_direct(payload: &Value) -> bool {
    payload.get("code").and_then(|v| v.as_str()) == Some("attach_direct")
}

/// The reason text `PtyOpenFailed` carries for a `pty.open` reply that
/// isn't `attach_direct` — the reply's own `code` field, or a generic
/// fallback when the payload carries none (a malformed frame, or a
/// success shape this build no longer expects).
fn pty_open_failure_reason(payload: &Value) -> String {
    payload
        .get("code")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unsupported daemon reply".to_string())
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


#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_down_ssh_host_costs_the_hub_at_most_two_logins_a_minute() {
        let count = |dial: &Dial| {
            let (mut t, mut b, mut n) = (0u64, 200u64, 0u32);
            while t < 3_600_000 {
                n += 1;
                t += b;
                b = next_backoff_ms(b, dial);
            }
            n
        };
        let ssh = Dial::Ssh(sot_protocol::ssh_bridge::SshRecipe::new("hub", Some("gamma")).unwrap());
        let n = count(&ssh);
        assert!(n <= 130, "{n} logins per hour on the hub for one down host");
        let n = count(&Dial::Pipe(std::path::PathBuf::from("/x")));
        assert!(n >= 700, "a local socket keeps its 5 s cap, got {n} probes per hour");
    }

    // --- ADR 0045 decision 4: the link gate. ---

    struct NoWindow;
    impl Redraw for NoWindow {
        fn request_redraw(&self) {}
    }

    /// A fake daemon on one end of an in-memory stream: answers the hello
    /// with `hello_reply`, then (when `answer_preamble`) answers the
    /// tree.root and preview.get preamble with an `{error}` payload, and
    /// holds the stream open until `hold` is dropped.
    async fn run_against_fake_daemon(
        host: &str,
        hello_reply: serde_json::Value,
        answer_preamble: bool,
        gate: sot_protocol::ssh_bridge::LinkGate,
    ) -> (
        tokio::task::JoinHandle<Result<()>>,
        tokio::task::JoinHandle<()>,
        std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let daemon = tokio::spawn(async move {
            let (rx, mut tx) = tokio::io::split(far);
            let mut rx = codec::buffered(rx);
            let (hello, _) = codec::read_frame(&mut rx).await.unwrap();
            codec::write_frame(&mut tx, &Frame::res(hello.id, op::HELLO, hello_reply).with_rev(0), None).await.unwrap();
            if answer_preamble {
                for _ in 0..2 {
                    let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                    let err = serde_json::json!({ "error": "cannot read the default directory", "code": "io" });
                    codec::write_frame(&mut tx, &Frame::res(req.id, &req.op, err).with_rev(0), None).await.unwrap();
                }
            }
            let _ = hold_rx.await;
        });
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let host = host.to_string();
        let session = tokio::spawn(async move {
            let (rx, tx) = tokio::io::split(near);
            let (_out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut backoff_ms = 200;
            let result = run_protocol(
                host.clone(),
                codec::buffered(rx),
                tx,
                None,
                &evt_tx,
                &mut out_rx,
                &NoWindow,
                &mut backoff_ms,
                ResolvedDial::Local,
                Some(&gate),
            )
            .await;
            result
        });
        (session, daemon, evt_rx, hold_tx)
    }

    fn hello_ok() -> serde_json::Value {
        serde_json::json!({ "session_id": "sess-1", "revision": 0, "snapshot_pending": false })
    }

    #[tokio::test]
    async fn the_gate_is_up_after_the_hello_reply_and_down_when_the_session_ends() {
        let _env = crate::state::test_env::set_test_env();
        let gate = sot_protocol::ssh_bridge::LinkGate::default();
        gate.set_up(false);
        let (session, daemon, evt_rx, hold) =
            run_against_fake_daemon("gate-test-up-down", hello_ok(), true, gate.clone()).await;
        let t0 = std::time::Instant::now();
        while evt_rx.try_recv().map_or(true, |(_, e)| !matches!(e, IncomingEvt::Connected { .. })) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "no Connected event");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(gate.is_up(), "a hello reply proves the link");
        drop(hold);
        daemon.await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session).await.unwrap().unwrap();
        assert!(result.is_err(), "the daemon closing ends the session");
        assert!(!gate.is_up(), "the gate is down by the time run_protocol has returned");
    }

    #[tokio::test]
    async fn a_protocol_mismatch_reply_leaves_the_gate_up() {
        let _env = crate::state::test_env::set_test_env();
        let gate = sot_protocol::ssh_bridge::LinkGate::default();
        gate.set_up(false);
        let reply = serde_json::json!({ "error": "protocol skew", "code": "protocol_mismatch" });
        let (session, _daemon, _evt_rx, _hold) =
            run_against_fake_daemon("gate-test-mismatch", reply, false, gate.clone()).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session).await.unwrap().unwrap();
        assert!(result.unwrap_err().is::<HelloRefused>());
        assert!(gate.is_up(), "a refusal is a reply: the link is up");
    }

    #[tokio::test]
    async fn a_tree_root_error_reply_does_not_end_the_session() {
        let _env = crate::state::test_env::set_test_env();
        let gate = sot_protocol::ssh_bridge::LinkGate::default();
        let (session, _daemon, _evt_rx, _hold) =
            run_against_fake_daemon("gate-test-tree-root", hello_ok(), true, gate.clone()).await;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(!session.is_finished(), "the session must still be connected after 1 s");
        assert!(gate.is_up());
        session.abort();
        let _ = session.await;
    }





    // --- Field incident 2026-09-08: a peer closing the LOCAL connection
    // must surface as an error, not a silent hang. ---

    /// End-to-end regression: when the PEER closes its end of the local
    /// connection, the transport's read path must surface that as an `Err`
    /// promptly — never hang — so
    /// `run_protocol`'s `read?` (see the steady-state loop's read arm)
    /// propagates it and `spawn`'s reconnect loop ("transport task ended;
    /// reconnecting") takes over. Drives a REAL `interprocess` local-socket
    /// listener/stream pair — the exact `connect_pipe`/`read_owned`
    /// functions `run_protocol` itself calls (a Unix domain socket on this
    /// platform, a Windows named pipe there, same code path) — not a mock.
    #[tokio::test]
    async fn a_closed_local_connection_surfaces_as_an_error_not_a_silent_hang() {
        use interprocess::local_socket::{tokio::prelude::*, GenericFilePath, ListenerOptions};

        let unique = format!(
            "sot-transport-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        // A named-pipe path on Windows, a socket file elsewhere -- both go
        // through `GenericFilePath`, exactly the route `connect_pipe` takes.
        #[cfg(windows)]
        let sock_path = std::path::PathBuf::from(format!(r"\\.\pipe\{unique}"));
        #[cfg(not(windows))]
        let sock_path = std::env::temp_dir().join(format!("{unique}.sock"));
        let _ = std::fs::remove_file(&sock_path);
        let name = sock_path
            .to_str()
            .unwrap()
            .to_fs_name::<GenericFilePath>()
            .unwrap();
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .expect("bind test socket");

        // Server: accept once, answer the hello handshake (so this exercises
        // a connection that was genuinely live, not merely refused), then
        // DROP the connection — standing in for the daemon closing its end
        // ("frame write exceeded 10s; dropping connection (peer not
        // draining)") in the field incident.
        let server = tokio::spawn(async move {
            let conn = listener.accept().await.expect("accept");
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (hello, _) = codec::read_frame(&mut rx).await.expect("read hello");
            let hello_res = serde_json::json!({
                "session_id": "sess-1",
                "revision": 0,
                "snapshot_pending": false,
            });
            codec::write_frame(
                &mut tx,
                &Frame::res(hello.id, op::HELLO, hello_res).with_rev(0),
                None,
            )
            .await
            .expect("write hello res");
            // Connection drops here (both halves go out of scope) — the
            // simulated server-side close.
        });

        let stream = connect_pipe(&sock_path).await.expect("client connect");
        let (client_rx, mut client_tx) = stream.split();
        let mut client_rx = codec::buffered(client_rx);
        codec::write_frame(
            &mut client_tx,
            &Frame::req(
                1,
                op::HELLO,
                serde_json::to_value(HelloReq {
                    client_id: "test-client".into(),
                    session_id: None,
                    last_seen_revision: 0,
                    token: None,
                    protocol: sot_protocol::PROTOCOL_VERSION,
                    app_version: sot_protocol::app_version(),
                    host: None,
                    role: String::new(),
                    instance: None,
                    name: None,
                })
                .unwrap(),
            ),
            None,
        )
        .await
        .expect("write hello req");
        let (hello_frame, _) = codec::read_frame(&mut client_rx).await.expect("read hello res");
        assert_eq!(hello_frame.id, 1);
        server.await.expect("server task must not panic");

        // The peer has now closed. `read_owned` is EXACTLY what the
        // steady-state select! loop polls (see `run_protocol`) — its next
        // completion must be an `Err` (EOF), bounded by a short timeout so
        // this test itself proves "promptly", not just "eventually".
        let (_rx_back, result) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_owned(client_rx),
        )
        .await
        .expect("the read must complete promptly once the peer has closed, not hang");

        assert!(
            result.is_err(),
            "a closed peer connection must surface as an Err, not hang forever"
        );

        let _ = std::fs::remove_file(&sock_path);
    }




    // --- ADR 0042 slice L1b: the attach_direct switch. ---

    #[test]
    fn is_attach_direct_declines_every_other_response_shape() {
        let attach = serde_json::json!({
            "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
            "code": "attach_direct",
            "state_dir": "/state/workspaces/ws-1",
        });
        assert!(is_attach_direct(&attach));
        // Still recognized when the daemon couldn't resolve a state root —
        // the code alone gates this now, not the (ignored) path.
        let attach_no_dir = serde_json::json!({
            "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
            "code": "attach_direct",
            "state_dir": serde_json::Value::Null,
        });
        assert!(is_attach_direct(&attach_no_dir));
        // Any other response shape — this build's daemon never sends
        // one for `pty.open`, but the check must still decline it.
        let ok = serde_json::json!({"cols": 80, "rows": 24});
        assert!(!is_attach_direct(&ok));
        // A DIFFERENT error code must not be mistaken for attach_direct —
        // only the exact literal switches the pane to the attach path.
        let other_error = serde_json::json!({"error": "boom", "code": "bad_target"});
        assert!(!is_attach_direct(&other_error));
    }

    #[test]
    fn pty_open_failure_reason_prefers_code_falls_back_when_absent() {
        let coded = serde_json::json!({"error": "boom", "code": "bad_target"});
        assert_eq!(pty_open_failure_reason(&coded), "bad_target");
        let uncoded = serde_json::json!({"cols": 80, "rows": 24});
        assert_eq!(pty_open_failure_reason(&uncoded), "unsupported daemon reply");
    }
}
