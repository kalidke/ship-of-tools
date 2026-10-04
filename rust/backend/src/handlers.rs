// handlers.rs — op dispatch for the M1 spike.
//
// Each handler takes a parsed Frame (the codec already verified envelope +
// blob), returns a (Frame, Option<Vec<u8>>) tuple the connection task writes
// back. Handlers borrow the Session for state mutations.
//
// All content here is hardcoded for the spike. The eventual kernel-driven
// path replaces these stubs with calls into ShipToolsKernel over its own pipe;
// the on-the-wire Frame shape stays the same.

use anyhow::Context;

use anyhow::Result;

use serde_json::json;

use sot_protocol::op;


























use sot_protocol::Frame;






use sot_protocol::PtyCursor;

use sot_protocol::PtyEnter;

use sot_protocol::PtyInputReq;

use sot_protocol::PtyInputRes;

use sot_protocol::PtyScreenReq;

use sot_protocol::PtyScreenRes;


















use crate::session::Session;



#[cfg(test)]
use crate::workspaces::Workspace;

use crate::workspaces::WorkspaceChanged;

use crate::workspaces::Workspaces;

use tokio::sync::broadcast;

pub(crate) use crate::server::reply::HandlerOutput;

pub(crate) use crate::files::preview::{crop::handle_image_crop, handle_preview_get, scale::handle_preview_set_scale};


pub(crate) use crate::server::hello::{handle_hello, constant_time_eq};

pub(crate) use crate::clients::{handle_fe_command_send, handle_fe_presence, handle_fe_sessions, handle_version_query};

pub(crate) use crate::files::tree_ops::{handle_directory_list, handle_nav_toggle_hidden, handle_tree_children, handle_tree_root};

pub(crate) use crate::files::concept_ops::{handle_concept_list, handle_concept_read, handle_concept_write};

pub(crate) use crate::files::io_ops::{handle_dir_create, handle_file_delete, handle_file_read, handle_file_write};

pub(crate) use crate::sidecars::repl::ops::{handle_repl_eval, handle_repl_interrupt, handle_repl_run_file};

pub(crate) use crate::sidecars::repl::execute::handle_repl_execute;

pub(crate) use crate::sidecars::ops::{handle_kernel_request, handle_math_render, handle_pluto_open};

/// Duplicate-root gate lookup (ADR 0036 Phase 1): the first registered
/// workspace whose `project_root` canonicalizes to `candidate_canon` while
/// carrying a slug OTHER than `incoming_slug` — excluding the inert default
/// anchor (`Workspaces::is_inert_default_anchor`, ADR 0042 amendment): it is
/// not a session and never runs an agent, so a real session at its root (a
/// local host's home dir) is not the two-agents-one-tree collision this gate
/// refuses. Same-slug matches are deliberately invisible here: a same-slug
/// create is decided by `same_slug_row_in_use`, which keeps the id-preserving
/// refresh only for a row not in use. A
/// registered root that no longer canonicalizes (deleted dir, dangling
/// symlink) is skipped, not fatal: judging that workspace is the Phase 2
/// reap's job, not the create path's.
fn find_other_workspace_with_root(
    candidate_canon: &std::path::Path,
    incoming_slug: &str,
    workspaces: &crate::workspaces::Workspaces,
) -> Option<std::sync::Arc<crate::workspaces::Workspace>> {
    workspaces.list().into_iter().find(|w| {
        w.slug != incoming_slug
            && !workspaces.is_inert_default_anchor(w)
            && w.project_root
                .canonicalize()
                .map(|c| c == candidate_canon)
                .unwrap_or(false)
    })
}

pub(crate) use crate::files::confine::{canonicalize_and_workspace_root, canonicalize_within_any_workspace};

pub(crate) use crate::pages::ops::{handle_docs_open, handle_quarto_open, handle_video_open};

pub(crate) use crate::paths::valid_name;

pub(crate) use crate::files::transfer::{handle_file_upload, stream_file_download};

/// `PtyInputReq::origin` / `PtyScreenReq` share no size limit of their own
/// — this one is `origin`'s: ADR 0042 amendment §1, "≤128 bytes, else
/// `bad_origin`."
const MAX_PTY_INPUT_ORIGIN_LEN: usize = 128;

/// One capsule-lane op's absolute deadline (ADR 0042 amendment: "the whole
/// operation runs under ONE deadline (5 s): attach, checkpoint, take,
/// input, ack, detach"). Shared by both `pty.input` and `pty.screen`'s
/// capsule arms, which are the only readers.
const CAPSULE_OP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Bounds for [`crate::capsule_workspace::headless::write_and_enter`]'s
/// pacing wait — never a confirmation, only pacing.
const CAPSULE_WRITE_QUIET_BUDGET: std::time::Duration = std::time::Duration::from_millis(300);
const CAPSULE_WRITE_PACING_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// What a capsule-runtime `pty.input`/`pty.screen` op's `spawn_blocking`
/// closure reports — computed OFF the async runtime (the phase probe and
/// the headless client both make blocking IPC calls), then translated to a
/// response frame back on the async side. Ungated, like
/// `capsule_workspace::headless` itself (macOS wiring lane): one variant
/// set, one outcome shape, on every host this daemon builds for.
enum CapsuleOpOutcome<T> {
    Ok(T),
    NotReady(&'static str),
    Headless(crate::capsule_workspace::headless::HeadlessError),
}

/// Translates a [`CapsuleOpOutcome::Headless`] error into the typed error
/// payload ADR 0042 amendment §2 pins. `fail_code` is `capsule_input_failed`
/// or `capsule_screen_failed` depending on the caller; a `phase` of
/// `"record"` — the daemon could not learn the record's own verdict,
/// whether because the wire said `input_delivery_unknown` or because the
/// deadline expired after the input had already been handed to the lane —
/// overrides it to `capsule_input_unknown` regardless (ADR 0042 amendment
/// §2's "capsule_input_unknown when the deadline expired AFTER the input
/// was submitted", generalized to the wire's own explicit "unknown" answer
/// too: both cases mean the same thing, "we do not know if this landed in
/// the record," and the daemon never retries either one on its own).
fn headless_error_payload(
    e: crate::capsule_workspace::headless::HeadlessError,
    fail_code: &'static str,
) -> serde_json::Value {
    let code = if e.phase == "record" { "capsule_input_unknown" } else { fail_code };
    json!({
        "error": e.detail,
        "code": code,
        "phase": e.phase,
        "submitted": e.submitted,
    })
}

/// Resolves `req.origin` into a controller id, or an early error frame for
/// `bad_origin` (ADR 0042 amendment §1/§2: `origin` is caller-supplied
/// ATTRIBUTION, never authentication — exactly `HelloReq::client_id`'s own
/// trust level, just named explicitly instead of read off the connection).
fn resolve_pty_input_controller_id(
    req_id: u64,
    op_str: &str,
    origin: Option<&str>,
    connection_client_id: &str,
) -> std::result::Result<String, Frame> {
    match origin {
        Some(o) if o.len() > MAX_PTY_INPUT_ORIGIN_LEN => {
            let payload = json!({
                "error": format!("origin exceeds {MAX_PTY_INPUT_ORIGIN_LEN} bytes"),
                "code": "bad_origin",
            });
            Err(Frame::res(req_id, op_str, payload))
        }
        Some(o) => Ok(o.to_string()),
        None => Ok(connection_client_id.to_string()),
    }
}

#[cfg(test)]
mod pty_input_controller_id_tests {
    use super::*;

    #[test]
    fn absent_origin_falls_back_to_the_connection_client_id() {
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, None, "conn-client").unwrap();
        assert_eq!(id, "conn-client");
    }

    #[test]
    fn present_origin_wins_over_the_connection_client_id() {
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some("host-4-dev"), "conn-client").unwrap();
        assert_eq!(id, "host-4-dev");
    }

    #[test]
    fn origin_at_exactly_the_bound_is_accepted() {
        let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN);
        let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client").unwrap();
        assert_eq!(id, origin);
    }

    #[test]
    fn origin_one_over_the_bound_is_bad_origin() {
        let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN + 1);
        let frame = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client")
            .expect_err("an over-length origin must be refused");
        assert_eq!(frame.payload["code"], "bad_origin");
    }
}

/// ADR 0042 amendment (2026-09-07), decision 1: a session types into
/// ANOTHER row's pane by `workspace_id`. Unlike `pty.write` (this
/// connection's own pty, fire-and-forget), this is ANSWERED — a caller
/// with no pane to look at needs the outcome. `controller_id` is the
/// connection's own `hello` `client_id`, used only when `req.origin` is
/// absent; both are attribution, never authentication (the record stores
/// who typed, how many bytes, and when — never the content, which is
/// redacted in the WAL — and this op grants no privilege either name alone
/// could forge).
pub async fn handle_pty_input(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
    connection_client_id: &str,
) -> Result<HandlerOutput> {
    let req: PtyInputReq = serde_json::from_value(payload_json).context("pty.input payload")?;

    let controller_id = match resolve_pty_input_controller_id(
        req_id,
        op::PTY_INPUT,
        req.origin.as_deref(),
        connection_client_id,
    ) {
        Ok(id) => id,
        Err(frame) => return Ok(vec![(frame, None)]),
    };

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
    };

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let bytes = match STANDARD.decode(req.data_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            let payload = json!({ "error": format!("data_b64: {e}"), "code": "bad_request" });
            return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
        }
    };

    match ws.runtime.as_str() {
        "capsule" => {
            {
                let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::capsule_workspace::STATE_ROOT_HINT
                        ),
                        "code": "capsule_input_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
                };
                let state_dir =
                    crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
                let enter = req.enter;
                // The ORIGINAL payload length — `PtyInputRes::bytes`'s own
                // doc ("the enter byte, if requested, is not counted"), so
                // this is captured BEFORE the CR (if any) is appended below.
                let byte_len = bytes.len();
                // ADR 0043 decision 33: `resume_if_absent` in place of a
                // bare `phase_of` read — a row whose supervisor died
                // between two headless ops resumes itself, under its own
                // guard, rather than answering `NotReady` forever. A
                // resume failure (an unknown workspace, a spawn error)
                // is logged and folds into `UNREACHABLE_PHASE`, same
                // shape `phase_of` itself always reported for a dead lane.
                let workspace_id = ws.workspace_id.clone();
                let agent_kind = ws.agent();
                let agent_name = ws.agent_name();
                let slug = ws.slug.clone();
                let project_root = ws.project_root.clone();
                let workspaces_for_resume = workspaces.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let phase = match crate::capsule_workspace::resume_if_absent(
                        &state_root,
                        &workspace_id,
                        &agent_kind,
                        &agent_name,
                        &slug,
                        &project_root,
                        workspaces_for_resume,
                    ) {
                        Ok(phase) => phase,
                        Err(e) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %e, "pty.input: resume_if_absent failed");
                            crate::capsule_workspace::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    // Split write+pace+enter: `write_and_enter`'s own doc.
                    if enter {
                        match crate::capsule_workspace::headless::write_and_enter(
                            &state_dir,
                            &controller_id,
                            &bytes,
                            CAPSULE_OP_DEADLINE,
                            CAPSULE_WRITE_QUIET_BUDGET,
                            CAPSULE_WRITE_PACING_BUDGET,
                        ) {
                            Ok((_n, enter)) => CapsuleOpOutcome::Ok(enter),
                            Err(e) => CapsuleOpOutcome::Headless(e),
                        }
                    } else {
                        let deadline = std::time::Instant::now() + CAPSULE_OP_DEADLINE;
                        match crate::capsule_workspace::headless::type_into(&state_dir, &controller_id, &bytes, deadline) {
                            Ok(_n) => CapsuleOpOutcome::Ok(PtyEnter::NotSent),
                            Err(e) => CapsuleOpOutcome::Headless(e),
                        }
                    }
                })
                .await
                .context("spawn_blocking pty.input capsule")?;
                match outcome {
                    CapsuleOpOutcome::Ok(enter) => {
                        let res = PtyInputRes { ok: true, runtime: "capsule".into(), bytes: byte_len, enter };
                        Ok(vec![(
                            Frame::res(req_id, op::PTY_INPUT, serde_json::to_value(res)?),
                            None,
                        )])
                    }
                    CapsuleOpOutcome::NotReady(phase) => {
                        let payload = json!({
                            "error": format!("capsule row not ready (phase: {phase})"),
                            "code": "capsule_not_ready",
                            "phase": phase,
                        });
                        Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
                    }
                    CapsuleOpOutcome::Headless(e) => {
                        let payload = headless_error_payload(e, "capsule_input_failed");
                        Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
                    }
                }
            }
        }
        other => {
            let payload = json!({
                "error": format!("workspace runtime {other:?} has no pty.input path"),
                "code": "runtime_not_available",
            });
            Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)])
        }
    }
}

/// ADR 0042 amendment (2026-09-07), decision 2: the CURRENT screen of a
/// named row — no scrollback, no history. Never takes the pen on a capsule
/// row (a WATCHER attach); never touches `tmux.capture_pane` (that op's
/// scrollback-including read stays exactly what it is, for its own
/// pane-id callers).
pub async fn handle_pty_screen(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: PtyScreenReq = serde_json::from_value(payload_json).context("pty.screen payload")?;

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)]);
    };

    match ws.runtime.as_str() {
        "capsule" => {
            {
                let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::capsule_workspace::STATE_ROOT_HINT
                        ),
                        "code": "capsule_screen_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)]);
                };
                let state_dir =
                    crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
                // A pure watcher never takes, so this id never lands in any
                // input record — it exists only because `FeAttachClient::
                // attach`'s signature takes one; a fixed, self-describing
                // constant is honester than fabricating an identity this
                // read has no caller-supplied handle for.
                let controller_id = "sot-fe-screen".to_string();
                // ADR 0043 decision 33: `resume_if_absent` in place of a
                // bare `phase_of` read — see `pty.input`'s own comment.
                let workspace_id = ws.workspace_id.clone();
                let agent_kind = ws.agent();
                let agent_name = ws.agent_name();
                let slug = ws.slug.clone();
                let project_root = ws.project_root.clone();
                let workspaces_for_resume = workspaces.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let phase = match crate::capsule_workspace::resume_if_absent(
                        &state_root,
                        &workspace_id,
                        &agent_kind,
                        &agent_name,
                        &slug,
                        &project_root,
                        workspaces_for_resume,
                    ) {
                        Ok(phase) => phase,
                        Err(e) => {
                            tracing::warn!(workspace_id = %workspace_id, error = %e, "pty.screen: resume_if_absent failed");
                            crate::capsule_workspace::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    let deadline = std::time::Instant::now() + CAPSULE_OP_DEADLINE;
                    match crate::capsule_workspace::headless::screen_of(
                        &state_dir,
                        &controller_id,
                        deadline,
                    ) {
                        Ok(shot) => CapsuleOpOutcome::Ok(shot),
                        Err(e) => CapsuleOpOutcome::Headless(e),
                    }
                })
                .await
                .context("spawn_blocking pty.screen capsule")?;
                match outcome {
                    CapsuleOpOutcome::Ok(shot) => {
                        let res = PtyScreenRes {
                            runtime: "capsule".into(),
                            cols: shot.cols,
                            rows: shot.rows,
                            lines: shot.lines,
                            cursor: shot.cursor.map(|(row, col)| PtyCursor { row, col }),
                        };
                        Ok(vec![(
                            Frame::res(req_id, op::PTY_SCREEN, serde_json::to_value(res)?),
                            None,
                        )])
                    }
                    CapsuleOpOutcome::NotReady(phase) => {
                        let payload = json!({
                            "error": format!("capsule row not ready (phase: {phase})"),
                            "code": "capsule_not_ready",
                            "phase": phase,
                        });
                        Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
                    }
                    CapsuleOpOutcome::Headless(e) => {
                        let payload = headless_error_payload(e, "capsule_screen_failed");
                        Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
                    }
                }
            }
        }
        other => {
            let payload = json!({
                "error": format!("workspace runtime {other:?} has no pty.screen path"),
                "code": "runtime_not_available",
            });
            Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)])
        }
    }
}

/// A same-slug `workspace.create` refreshes the existing row in place
/// (`Workspaces::insert` keeps its id, new metadata wins) and then starts it
/// in `StartMode::Start`, which is right only for a row no supervisor has
/// published to. Any observed phase means a supervisor holds the row (a second
/// leg exits contended, 70) and the refresh would rewrite the account, agent
/// and task of a run that keeps its old ones; a non-capsule row would get a
/// capsule started beside it. So the refresh is kept only for a capsule row
/// still in phase `stopped`; this returns any other same-slug row.
fn same_slug_row_in_use(
    incoming_slug: &str,
    workspaces: &Workspaces,
) -> Option<std::sync::Arc<crate::workspaces::Workspace>> {
    workspaces.list().into_iter().find(|w| {
        w.slug == incoming_slug
            && (w.runtime != "capsule" || w.phase() != crate::workspaces::Phase::Stopped)
    })
}

pub async fn handle_workspace_create(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceCreateReq, WorkspaceCreateRes};
    // ADR 0023 §3 daemon-boot trigger — read off the raw payload (it is not a
    // `WorkspaceCreateReq` struct field: adding one would force the frozen FE's
    // struct literal to set it). serde ignores it on the typed deserialize below.
    let boot = payload_json
        .get("boot")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let req: WorkspaceCreateReq =
        serde_json::from_value(payload_json).context("workspace.create payload")?;
    tracing::info!(label = %req.label, project_root = %req.project_root, boot, "workspace.create");

    let project_root = std::path::PathBuf::from(&req.project_root);
    if !project_root.exists() {
        let payload = json!({
            "error": format!("project_root does not exist: {}", req.project_root),
            "code": "no_such_path",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }
    if !project_root.is_dir() {
        let payload = json!({
            "error": format!("project_root is not a directory: {}", req.project_root),
            "code": "not_a_directory",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Duplicate-root gate (ADR 0036 Phase 1): one project root, one workspace
    // identity. A second registration for an already-registered root would
    // persist a TOML the daemon then faithfully respawns on every boot, and
    // hands two agent sessions one shared working tree (the collision class
    // worktrees exist to prevent). Compared by canonical path on BOTH sides so
    // symlinked spellings of one directory still collide; refused only for a
    // DIFFERENT slug (a same-slug create is the in-use gate's question, just
    // below). The `existing` block lets the caller offer
    // "switch to that workspace" instead of dead-ending. Canonicalization
    // failure on the candidate skips the gate rather than failing the create —
    // prevention must not make creation less reliable than it is today.
    let incoming_slug = crate::paths::slug(&req.label);
    match project_root.canonicalize() {
        Ok(canon) => {
            if let Some(existing) =
                find_other_workspace_with_root(&canon, &incoming_slug, workspaces)
            {
                let payload = json!({
                    "error": format!(
                        "project_root is already registered as workspace '{}' (slug '{}')",
                        existing.label, existing.slug
                    ),
                    "code": "duplicate_root",
                    "existing": {
                        "workspace_id": existing.workspace_id,
                        "slug": existing.slug,
                        "label": existing.label,
                    },
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, project_root = %req.project_root,
                "duplicate-root gate skipped — candidate did not canonicalize");
        }
    }

    // In-use gate (see `same_slug_row_in_use`): refused before any state
    // changes. `workspace_id` stays nested under `existing` -- the frontend
    // reads a top-level `workspace_id` as success (transport.rs).
    if let Some(existing) = same_slug_row_in_use(&incoming_slug, workspaces) {
        let phase = existing.phase().as_wire_str();
        let payload = json!({
            "error": format!(
                "workspace '{}' (slug '{}') is in use ({} row, phase '{}'): attach to it, or destroy it before creating it again",
                existing.label, existing.slug, existing.runtime, phase
            ),
            "code": "label_in_use",
            "existing": {
                "workspace_id": existing.workspace_id,
                "slug": existing.slug,
                "label": existing.label,
                "phase": phase,
            },
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Name validation (security review): `agent_name` is persisted and later
    // spliced RAW (no quoting) into a shell command string by
    // `pty::boot_wrapper_command` (`export SOT_COMM_NAME={agent_name}; …`).
    // Empty is a legitimate "no agent name" sentinel (boot_wrapper_command
    // skips the export then); anything non-empty must match the strict
    // allowlist or this is rejected outright rather than silently sanitized.
    if !req.agent_name.is_empty() && !valid_name(&req.agent_name) {
        let payload = json!({
            "error": format!(
                "invalid agent_name {:?} (want 1-64 chars of [A-Za-z0-9._-])",
                req.agent_name
            ),
            "code": "bad_agent_name",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Register the workspace in memory + on disk first; the tmux session
    // is a UX nicety that the user can always re-create later, so we
    // don't fail the op if tmux misbehaves.
    // ADR 0031: resolve the agent kind. Explicit `agent` wins; absent derives
    // from the legacy `autostart_claude` flag.
    let agent_kind: String = if !req.agent.is_empty() {
        match req.agent.as_str() {
            "claude" | "codex" | "none" => req.agent.clone(),
            other => {
                let payload = json!({
                    "error": format!("unknown agent kind '{other}' (want claude | codex | none)"),
                    "code": "bad_agent",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else if req.autostart_claude {
        "claude".to_string()
    } else {
        "none".to_string()
    };
    let autostart = agent_kind != "none";

    // ADR 0042's rule, flipped here (L6 / this repo's B6 lane) now that
    // the bridge (ADR 0045) gives a capsule row a remote attach path:
    // `""` (absent on the wire) resolves to "capsule". `"capsule"` still
    // asks for one explicitly either way. ADR 0046 decision 5: nothing
    // NEW runs on tmux — an explicit `"tmux"` ask is refused (Windows
    // already refused it; this extends the SAME refusal everywhere).
    // Existing tmux rows keep running unaffected; the daemon just stops
    // CREATING new ones — they retire by attrition.
    let runtime: String = match req.runtime.as_str() {
        "" | "capsule" => "capsule".to_string(),
        other => {
            let payload = json!({
                "error": format!("unknown runtime {other:?} (want \"capsule\" or \"\")"),
                "code": "bad_runtime",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    };
    // ADR 0042 slice L1a, Codex review finding 9: validated BEFORE any
    // state mutation, whenever the resolved runtime is "capsule" (every
    // NEW workspace on Windows, or an explicitly requested one anywhere
    // `capsule_workspace::runtime` compiles — ADR 0043 decision 22). The
    // tmux path below accepts every agent kind unchanged. `agent_argv` is
    // the same function the spawn itself uses, so this is the real
    // check, not a second guess at it — "codex" (no known launcher on
    // either platform) is refused here rather than silently launching a
    // bare shell nobody asked for. Codex's check is a plain file read (no
    // spawn), so this stays a direct call, no `spawn_blocking`.
    let capsule_argv: Vec<String> = if runtime == "capsule" {
        match crate::capsule_workspace::agent_argv(&agent_kind, Some(project_root.as_path())) {
            Ok(argv) => argv,
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "unsupported_agent_on_this_host",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        Vec::new()
    };
    // ADR 0043 decision 23: refuse an unqualified state root at the SAME
    // "before any state mutation" moment `capsule_argv` above already
    // established — before `ws_seed`, before `workspaces.insert`, before
    // any toml. Ungated, like the capsule spawn further down (macOS
    // wiring lane): there is no host this daemon builds for that lacks a
    // capsule runtime, so there is no second, platform-shaped refusal for
    // this check to defer to.
    let capsule_state_root: Option<std::path::PathBuf> = if runtime == "capsule" {
        match crate::capsule_workspace::qualified_state_root() {
            Ok(root) => Some(root),
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "state_root_unqualified",
                });
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        None
    };
    // A second refusal at the same before-any-mutation moment: a state
    // root resolving INSIDE this workspace's own project root would sit
    // under this daemon's project-root file watcher, whose open
    // directory handles block a Windows rename underneath them (field
    // defect: `sot-capsule supervise` exiting terminal 69 on
    // `MoveFileExW`). Same predicate `spawn_detached_supervisor` checks
    // again right before it spawns; this copy just gets a clean `code`
    // here instead of a rollback after a partial row insert.
    if let Some(root) = &capsule_state_root {
        if crate::capsule_workspace::state_root_inside_project(root, &project_root) {
            let payload = json!({
                "error": format!(
                    "state root {root:?} lies inside the project root {project_root:?}: a \
                     capsule's state tree must never sit inside a directory this workspace watches"
                ),
                "code": "state_root_inside_project",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    // Accounts brief (v0.6.0): resolved once, HERE, and recorded on the
    // row — never re-derived later. `""`/absent is the default account,
    // a no-op. A non-default account is checked with the SAME pure
    // resolver the spawn path itself calls ([`crate::accounts::account_env`]),
    // so a create-time refusal and a later spawn-time one (the folder
    // vanishing in between) can never disagree. Refuses loudly, before
    // any state mutation (the same moment `capsule_argv`/the state-root
    // checks above already established) — never a silent fallback to
    // the default folder: a bash (`agent == "none"`) row is refused the
    // same way a claude/codex row with a missing folder is, both
    // surfacing `account_env`'s own exact-command message.
    let account: String = req.account.clone().unwrap_or_default();
    if !account.is_empty() && account != "default" {
        let home = crate::accounts::account_home();
        let check = home
            .ok_or_else(|| "no home directory to resolve an account against".to_string())
            .and_then(|home| crate::accounts::account_env(&agent_kind, &account, &home));
        if let Err(detail) = check {
            let payload = json!({
                "error": detail,
                "code": "unknown_account",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    let mut ws_seed = crate::workspaces::Workspace::from_label(
        &req.label,
        project_root.clone(),
        autostart,
        agent_kind.clone(),
        req.agent_name.clone(),
        req.task.clone(),
    );
    ws_seed.runtime = runtime;
    ws_seed.account = std::sync::Mutex::new(account);
    // The run gate, before the row exists: a refused create leaves nothing
    // to roll back. Held through the start below, so a shutdown that
    // closes the gate meanwhile waits for this create to finish.
    let start_permit = match workspaces.begin_start(&ws_seed.workspace_id) {
        Ok(permit) => permit,
        Err(refusal) => {
            let payload = json!({
                "error": format!("capsule workspace could not be started: {refusal}"),
                "code": "capsule_spawn_failed",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    };
    let ws_handle = workspaces.insert(ws_seed);
    if let Err(e) = crate::workspaces::save(&ws_handle) {
        tracing::warn!(error = %e, "workspace toml persist failed; workspace is in-memory only");
    }

    // ADR 0043 decision 22: branch on the resolved runtime VALUE, not a
    // platform cfg. Since ADR 0046 decision 5 that value is always
    // "capsule" for a NEW row, and since the macOS wiring lane the
    // capsule runtime carries no platform gate at all: one spawn path,
    // every host this daemon builds for. What differs per platform lives
    // in `capsule_workspace`'s own leaf `cfg(unix)`/`cfg(windows)` arms,
    // so a host with neither fails to COMPILE rather than quietly
    // creating a row it can never supervise.
    {
    // ADR 0042 slice L1a, Codex review finding 1: the capsule spawn —
    // and, unlike the tmux path below, a SYNCHRONOUS failure here
    // FAILS the whole op: "a capsule workspace with no supervisor is
    // not a workspace." Rule C (shrink round): this daemon no longer
    // creates the state directory itself — `sot-capsule supervise`
    // creates its OWN, as its first act after it actually runs — so
    // a synchronous failure below leaves nothing on disk at all, not
    // even an empty directory. The DETACHED spawn-and-watch is what
    // survives this daemon's own exit, with its own exit handled
    // going forward (finding 6). On ANY failure to reach a running
    // supervisor, roll back the registry row and its persisted toml
    // and refuse the op with the real error text.
    // ADR 0043 decision 23: `capsule_state_root` was already resolved
    // and qualified ABOVE, before this row (or its toml) ever existed
    // — reuse it rather than re-resolving a second time. Always
    // `Some` here in practice (this arm only runs when
    // `ws_handle.runtime == "capsule"`, which is exactly when the
    // earlier check ran and would have already returned on failure);
    // the `None` arm stays as a defensive fallback, never actually hit.
    // ADR 0043 decision 29: a process spawn never runs on a Tokio
    // worker.
    //
    // ADR 0043 decision 33 (Codex review, 2026-09-11): this row's own
    // guard, taken HERE — inside the capsule arm only, never for a
    // tmux row (nothing else ever contends a tmux id's guard) — and
    // held across the spawn attempt below, closing the exact race
    // `pty.open`'s own `ensure_started` could otherwise win against
    // this handler's still-in-flight spawn (the field latency map's
    // own ordering: `ensure_started` can reach this SAME
    // freshly-minted workspace_id within milliseconds of the row
    // becoming visible via `insert` above). Every other lifecycle
    // mutation of a capsule row takes the SAME guard (`ensure_started`,
    // `resume_if_absent`, the watchdog's own restart, `resume_all`) —
    // this is that discipline's create-time entry. `capsule_guard`
    // itself already refuses to mint a guard for an absent row; the
    // membership recheck right after (under the lock, not before it)
    // catches one that vanished WHILE this waited for it — deciding
    // under the guard rather than starting unconditionally, the same
    // discipline every other guarded mutation follows.
    let capsule_guard = workspaces.capsule_guard(&ws_handle.workspace_id);
    let _capsule_guard_held = match &capsule_guard {
        Some(g) => Some(g.lock().await),
        None => None,
    };
    let still_registered = capsule_guard.is_some()
        && workspaces.list().iter().any(|ws| ws.workspace_id == ws_handle.workspace_id);
    let spawn_result: std::result::Result<(), String> = if !still_registered {
        Err("workspace was removed before its capsule supervisor could be started".to_string())
    } else {
        match capsule_state_root {
        None => Err(format!(
            "could not resolve this machine's state root ({} unset)",
            crate::capsule_workspace::STATE_ROOT_HINT
        )),
        // `&req.agent_name` verbatim (Codex round finding 2: no
        // synthesized default — a synthesized `<slug>-<host>` handed
        // to SOT_COMM_NAME would become an explicit pin that
        // overwrites any existing registry row of that name,
        // violating PROTOCOL.md's "never reuse a handle"; an empty
        // `agent_name` is a real, supported case now — comm-join.sh's
        // own #148 auto-disambiguating derivation picks the handle,
        // via the SOT_COMM_SELF_FILE this spawn pins).
        Some(state_root) => {
            let workspace_id = ws_handle.workspace_id.clone();
            let capsule_argv = capsule_argv.clone();
            let project_root = project_root.clone();
            let agent_name = req.agent_name.clone();
            let slug = ws_handle.slug.clone();
            let workspaces_for_spawn = workspaces.clone();
            // BLOCKING (process spawn, superseded by ADR 0045: no
            // pre-spawn probe runs here anymore): the row guard is
            // held by the CALLING async fn's own frame for this whole
            // `.await`, not by this closure — a panic in here is
            // caught by `spawn_blocking` itself and never unwinds
            // past that guard, so there is nothing to release on the
            // error path below beyond reporting it.
            tokio::task::spawn_blocking(move || {
                crate::capsule_workspace::start_supervisor(
                    &state_root,
                    &workspace_id,
                    crate::capsule_workspace::StartMode::Start,
                    &capsule_argv,
                    &project_root,
                    &agent_name,
                    &slug,
                    workspaces_for_spawn,
                )
            })
            .await
            .unwrap_or_else(|join_err| Err(format!("capsule spawn task panicked: {join_err}")))
            .map(|_phase| ())
        }
        }
    };
    match spawn_result {
        Ok(()) => {
            tracing::info!(workspace_id = %ws_handle.workspace_id, "workspace.create: capsule supervisor spawned");
            // Starts this row's lifecycle observer for its ongoing periodic poll.
            crate::capsule_workspace::observer::ensure_running(&workspaces, &ws_handle);
        }
        Err(detail) => {
            tracing::warn!(workspace_id = %ws_handle.workspace_id, error = %detail, "workspace.create: capsule spawn failed; rolling back");
            let _ = workspaces.remove_by_id(&ws_handle.workspace_id);
            for toml_path in [
                crate::workspaces::toml_path_for(&ws_handle.slug),
                crate::workspaces::legacy_toml_path_for(&ws_handle.slug),
            ] {
                match std::fs::remove_file(&toml_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!(error = %e, path = ?toml_path, "workspace.create rollback: toml remove failed"),
                }
            }
            let payload = json!({
                "error": format!("capsule workspace could not be started: {detail}"),
                "code": "capsule_spawn_failed",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    }
    drop(start_permit);

    let res = WorkspaceCreateRes {
        workspace_id: ws_handle.workspace_id.clone(),
        slug: ws_handle.slug.clone(),
        label: ws_handle.label.clone(),
        project_root: ws_handle.project_root.to_string_lossy().into_owned(),
        session_name: ws_handle.session_name.clone(),
    };
    let rev = session
        .bump(
            "workspace.created",
            json!({ "workspace_id": ws_handle.workspace_id, "slug": ws_handle.slug }),
        )
        .await;
    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll. Send error means no subscribers;
    // harmless.
    let _ = ws_events.send(WorkspaceChanged {
        action: "created".into(),
        slug: ws_handle.slug.clone(),
        workspace_id: ws_handle.workspace_id.clone(),
    });
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_CREATE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// ADR 0042 slice L1a (Codex review finding 3): whether a capsule
/// workspace's row (and its persisted toml) may be safely removed by
/// `workspace.destroy`.
pub(crate) enum CapsuleDestroyOutcome {
    /// The run was CONFIRMED ended (`RecordVerified`/`RecordClosed`/
    /// `AlreadyEnded` — see `capsule_workspace::EndRunOutcome`) — the row
    /// may be removed; the state directory never is. Human-readable
    /// (never the raw, Windows-only `EndRunOutcome` type) so this enum
    /// stays portable and unit-testable.
    Removable(String),
    /// Not confirmed (unreachable/starting/failed/refused/unknown) — the
    /// row and toml MUST be kept: never orphan a live run, never claim
    /// "ended" for one that wasn't.
    Kept { detail: String },
    /// Another path removed the row before this end reached its run: not
    /// ours to end, and nothing of it is kept.
    AlreadyRemoved,
}

/// What `workspace.destroy` answers for [`CapsuleDestroyOutcome::AlreadyRemoved`].
const ALREADY_REMOVED: &str = "workspace was removed before its capsule run could be ended";

/// Maps a `capsule_workspace::EndRunOutcome` to whether `workspace.destroy`
/// may remove the row. Pure/portable so it's unit-testable without a real
/// Windows lane; `#[cfg(test)]` below is its only caller off Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn capsule_destroy_outcome_of(o: crate::capsule_workspace::EndRunOutcome) -> CapsuleDestroyOutcome {
    use crate::capsule_workspace::EndRunOutcome as O;
    match o {
        O::RecordVerified => CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        O::RecordClosed => CapsuleDestroyOutcome::Removable(
            "run ended (record closed, not yet verified)".to_string(),
        ),
        O::AlreadyEnded => CapsuleDestroyOutcome::Removable("run had already ended".to_string()),
        // A `Terminal` authority has no leg left to orphan -- `end_run`
        // already sent it `stop` and waited for confirmed exit (see
        // `EndRunOutcome::Terminal`'s own doc). Without this arm a
        // capsule row whose agent argv can never launch was UNENDABLE:
        // `end_run` used to report this as `NotEnded` (kept) forever.
        O::Terminal => CapsuleDestroyOutcome::Removable(
            "the run was terminal; the supervisor was stopped".to_string(),
        ),
        // A `Starting` lane is NOT "not running" -- retryable.
        O::Starting => CapsuleDestroyOutcome::Kept {
            detail: "supervisor is starting; retry".to_string(),
        },
        O::NotEnded(detail) => CapsuleDestroyOutcome::Kept { detail },
        // The lane was unreachable but the supervisor lock itself was
        // free to take -- nobody holds this row (see `EndRunOutcome::
        // Unheld`'s own doc). A run with no holder is not running.
        O::Unheld => {
            CapsuleDestroyOutcome::Removable("no supervisor held the row".to_string())
        }
        // No state dir, no reachable lane at any point this daemon could
        // check (see `EndRunOutcome::Orphaned`'s own doc) -- its own
        // code, distinct from `Unheld`'s "no supervisor held the row":
        // this row never ran under this daemon's state root at all.
        O::Orphaned => CapsuleDestroyOutcome::Removable("orphan_removed".to_string()),
    }
}

/// After a default row's run is CONFIRMED ended (`confirmed_ended` from
/// `default_row_end_response`), reset the row's `agent`/`agent_name` back
/// to the inert-anchor shape and persist + broadcast the change — the
/// ADR 0042 amendment invariant ("an anchor with no run is inert, and
/// inert anchors are hidden") applied to the one path that used to leave
/// a carried-over `agent` stuck forever (field defect, v0.6.0-rc.12: the
/// owner once started an agent in this row before that rule existed, and
/// nothing ever reset `agent` back to "none" once its run ended, so
/// `Workspaces::is_inert_default_anchor` never went true again). A
/// `false` confirmed_ended is a no-op: `default_row_end_response` already
/// built the typed-error response for a `Kept` outcome, and neither the
/// row nor its toml may change under a refusal.
///
/// ADR 0043 decision 35: also prunes the row's sot-comm registry entries,
/// the same way `workspace.destroy`'s non-default path does below — a
/// killed default-row agent can't run its own `comm-leave`, so without
/// this its row lingered as a ghost `workspace.list` merges back in.
///
/// `held_guard` is `destroy_capsule_workspace`'s own row guard, carried
/// through unexamined so it stays locked across the reset below too
/// (ADR 0043 decision 33, Codex review round 2) — dropped only once this
/// function returns, whichever arm it takes.
pub(crate) async fn end_default_row_run(
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    workspace_id: &str,
    slug: &str,
    agent_name: &str,
    confirmed_ended: bool,
    _held_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
) {
    if !confirmed_ended {
        return;
    }
    let reg_agent = agent_name.to_string();
    let reg_ws = workspace_id.to_string();
    let reg_host = crate::workspaces::declared_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for the default row's ended run"
        );
    }
    if let Some(reset) = workspaces.reset_agent_to_none(workspace_id) {
        if let Err(e) = crate::workspaces::save(&reset) {
            tracing::warn!(error = %e, workspace_id = %workspace_id,
                "default row agent-reset toml persist failed; workspace is in-memory only");
        }
    }
    // Live-push so the Sessions strip re-lists — the row's phase is
    // derived fresh from the supervisor lane on every `workspace.list`
    // call. `action` is informational only: every `workspace.changed`
    // push just triggers an FE re-list.
    let _ = ws_events.send(WorkspaceChanged {
        action: "run_ended".into(),
        slug: slug.to_string(),
        workspace_id: workspace_id.to_string(),
    });
}

/// `reason` is the immutable end-run reason recorded on the wire —
/// parameterized so each caller (a real delete vs. the default row's
/// own kept-not-deleted branch) supplies its own honest text.
/// `agent_kind`/`agent_name`/`slug`/`project_root` are `ws`'s own fields,
/// passed through (rather than re-resolved) so this can call
/// `capsule_workspace::resume_locked` — the guard-free inner
/// `resume_if_absent` itself uses — under the SAME row guard `end_run`
/// then runs under (ADR 0043 decision 33's own resume-before-end
/// destroy caller): a row whose supervisor died leaves a live LEG behind
/// with no authority to end it; resuming re-establishes the authority so
/// `end_run` has a real lane to ask, rather than falling straight to its
/// own fence/leg proof. A resume failure is logged and never fails the
/// call — `end_run`'s own arms decide the outcome regardless.
/// `resume_first` false skips that resume: a window's close ends rows
/// without resuming any (`shutdown::end_rows`).
///
/// Every mutation runs under the row's own guard, from the first probe
/// through the outcome this returns — a terminal `Phase` mark alone
/// proves the authority exited, never that a leg is also gone.
///
/// Returns the row's own guard alongside the outcome, still HELD
/// (`None` only when no real lane call was ever attempted) — Codex
/// review round 2 on the L1a PR: an owned watchdog can check membership,
/// enter its own backoff, and restart the very row a caller is mid-way
/// through removing, unless the SAME guard covers both the end/stop
/// call here AND whatever the caller does with a confirmed outcome
/// (row removal, or the default row's own reset) afterward. The caller
/// holds it through that follow-up, then drops it.
pub(crate) async fn destroy_capsule_workspace(
    workspace_id: &str,
    reason: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &std::path::Path,
    workspaces: &Workspaces,
    resume_first: bool,
) -> (CapsuleDestroyOutcome, Option<tokio::sync::OwnedMutexGuard<()>>) {
    {
        let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
            return (
                CapsuleDestroyOutcome::Kept {
                    detail: format!(
                        "could not resolve this machine's state root ({} unset)",
                        crate::capsule_workspace::STATE_ROOT_HINT
                    ),
                },
                None,
            );
        };
        // SAFETY (Fable review): `sot_log::state_dir::state_dir_hash`
        // canonicalizes `state_dir` itself, falling back to the RAW path
        // only when that fails -- which is exactly the missing-directory
        // case the orphan proof exists for. If `state_root` is reached
        // through a symlink, a live supervisor (spawned while its own
        // `state_dir` existed) canonicalized the FULL real path and
        // bound its lane there; dialing the raw, non-canonical path
        // here would miss it -- ENOENT for the wrong reason, not because
        // nothing is running. Canonicalizing the ROOT (which, unlike the
        // row's own `state_dir`, is expected to exist) before joining
        // the workspace id closes this: the joined path then matches
        // what a live supervisor's own canonicalize would have produced,
        // whether or not this row's own `state_dir` still exists. When
        // the root itself cannot be canonicalized, `root_canonicalized`
        // is `false` and `end_run` must never attempt the orphan proof
        // on this call -- it keeps today's unconditional refusal instead
        // of trusting a hash built from an unresolved path.
        let (state_root, root_canonicalized) = match state_root.canonicalize() {
            Ok(canonical) => (canonical, true),
            Err(e) => {
                tracing::debug!(
                    state_root = ?state_root, error = %e,
                    "workspace.destroy: state root did not canonicalize; the orphan proof is refused this call"
                );
                (state_root, false)
            }
        };
        let state_dir = crate::capsule_workspace::state_dir_for(&state_root, workspace_id);
        let reason = reason.to_string();
        let workspace_id = workspace_id.to_string();
        let agent_kind = agent_kind.to_string();
        let agent_name = agent_name.to_string();
        let slug = slug.to_string();
        let project_root = project_root.to_path_buf();
        let workspaces_for_guard = workspaces.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            // ADR 0043 decision 33: this row's own guard, taken OWNED so
            // it survives this closure's return and stays held by the
            // caller through the row's actual removal/reset — see this
            // function's own doc. `None` (Codex review, 2026-09-11:
            // `capsule_guard` itself now refuses to mint one for a row
            // that is not currently registered) means a concurrent
            // remover already won this race — nothing left here to end.
            let Some(guard) = workspaces_for_guard.capsule_guard(&workspace_id) else {
                return (
                    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "unknown workspace")),
                    None,
                );
            };
            let held = guard.blocking_lock_owned();
            // Without the resume, its membership recheck still runs, so the
            // "row already gone" arm below holds for an end with no resume.
            let resumed = if resume_first {
                crate::capsule_workspace::resume_locked(
                    &state_root,
                    &workspace_id,
                    &agent_kind,
                    &agent_name,
                    &slug,
                    &project_root,
                    workspaces_for_guard.clone(),
                )
            } else {
                workspaces_for_guard
                    .resolve(Some(&workspace_id))
                    .map(|_| "not resumed")
                    .ok_or_else(|| "unknown workspace".to_string())
            };
            match resumed {
                // BLOCKER (Codex review, 2026-09-11): a pending resume can
                // outlive deletion. `resume_locked` returns this exact
                // sentinel phase ONLY when it just spawned a fresh
                // authority (its own probe first read `UNREACHABLE_PHASE`)
                // and `start_supervisor`'s settle deadline elapsed with
                // the lane STILL unobserved — an unresolved spawn is still
                // in flight under THIS SAME guard. Falling through to
                // `end_run` regardless (the old behaviour) would race it:
                // the freshly spawned process has not yet taken the fence
                // or re-executed the leg, so `end_run`'s own absence proof
                // could read both as acquirable and report the row
                // Removable an instant before that supervisor starts.
                // There is no cheap way to cancel or reap it from here —
                // the spawned `Child` is already owned by its own
                // watchdog, installed inside `resume_locked`'s own call,
                // never handed back to this caller — so a timeout stays
                // non-removable: `Kept` with an honest code
                // (`supervisor_starting`), never a guess.
                Ok(phase) if phase == crate::capsule_workspace::UNREACHABLE_PHASE => {
                    return (
                        Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "supervisor_starting")),
                        Some(held),
                    );
                }
                Ok(_) => {}
                // SHOULD-FIX (Codex review, 2026-09-11): a destroy that
                // waited behind another remover's SAME guard must not
                // continue into `end_run` once THIS recheck (run only
                // after the guard was actually acquired) finds the row
                // already gone — the old state dir's fence and leg really
                // are free once nothing owns it any more, so `end_run`'s
                // own proof would still succeed and report `Removable`,
                // and the caller would then delete a SLUG-keyed toml that
                // may since belong to a REPLACEMENT registration under
                // the same slug. `held` is dropped (not carried) so this
                // lands on the SAME "row already gone" `NotFound` arm
                // below the top-of-function race already uses.
                Err(e) if e == "unknown workspace" => {
                    return (Err(std::io::Error::new(std::io::ErrorKind::NotFound, e)), None);
                }
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %workspace_id, error = %e,
                        "workspace.destroy: resume before end_run failed; end_run's own arms decide"
                    );
                }
            }
            let result = crate::capsule_workspace::end_run(&state_dir, &reason, root_canonicalized);
            (result, Some(held))
        })
        .await;
        match outcome {
            Ok((Ok(o), held)) => (capsule_destroy_outcome_of(o), held),
            // `end_run`'s own `state_dir_missing` (ADR 0043 decision 33's
            // destroy proof: a missing state dir proves nothing and is
            // reported, never recreated) gets its own typed code rather
            // than folding into the generic "lane unreachable" detail —
            // `capsule_end_not_reached_payload` reads it back off this
            // exact sentinel string. A `None` guard here is the "row
            // already gone" race above, reusing the SAME NotFound kind —
            // never mistaken for a missing state dir.
            Ok((Err(e), held)) if held.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
                (CapsuleDestroyOutcome::AlreadyRemoved, None)
            }
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::NotFound => {
                (CapsuleDestroyOutcome::Kept { detail: "state_dir_missing".to_string() }, held)
            }
            // The pending-resume sentinel above — a timeout stays
            // non-removable with its own honest code, never folded into
            // the generic "supervisor lane unreachable" catch-all below.
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::WouldBlock => (
                CapsuleDestroyOutcome::Kept { detail: "supervisor_starting".to_string() },
                held,
            ),
            Ok((Err(e), held)) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("supervisor lane unreachable: {e}"),
                },
                held,
            ),
            Err(join_err) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("end_run task panicked: {join_err}"),
                },
                None,
            ),
        }
    }
}

/// The typed error `workspace.destroy` returns for a `Kept` outcome —
/// shared by the non-default path and the default row's own branch.
/// `"state_dir_missing"` and `"supervisor_starting"` are
/// `destroy_capsule_workspace`'s own sentinel details (ADR 0043 decision
/// 33) — the two `Kept` reasons with a code more specific than the
/// generic catch-all, so a caller can tell "nothing durable was ever
/// established here" and "a resume is still in flight, retry" apart from
/// every other kept reason without parsing prose.
fn capsule_end_not_reached_payload(detail: &str) -> serde_json::Value {
    let code = match detail {
        "state_dir_missing" => "state_dir_missing",
        "supervisor_starting" => "supervisor_starting",
        _ => "capsule_end_not_reached",
    };
    json!({
        "error": format!("capsule workspace could not be safely deleted: {detail}"),
        "code": code,
    })
}

/// The default row's own `workspace.destroy` response, built from an
/// already-computed outcome (pure/portable, unit-testable without a real
/// lane). Returns the payload and whether to broadcast `run_ended` —
/// `true` only for a CONFIRMED end; `Kept` gets the typed error instead.
fn default_row_end_response(
    workspace_id: &str,
    slug: &str,
    label: &str,
    outcome: CapsuleDestroyOutcome,
) -> (serde_json::Value, bool) {
    match outcome {
        CapsuleDestroyOutcome::Removable(detail) => {
            let res = sot_protocol::WorkspaceDestroyRes {
                workspace_id: workspace_id.to_string(),
                slug: slug.to_string(),
                label: label.to_string(),
                tmux_killed: false,
                toml_removed: false,
                kept: Some(format!("ended run of '{label}' ({detail})")),
            };
            (
                serde_json::to_value(res).expect("WorkspaceDestroyRes always serializes"),
                true,
            )
        }
        CapsuleDestroyOutcome::Kept { detail } => (capsule_end_not_reached_payload(&detail), false),
        CapsuleDestroyOutcome::AlreadyRemoved => (capsule_end_not_reached_payload(ALREADY_REMOVED), false),
    }
}

/// Remove a row's tomls from disk so neither registration path brings the
/// workspace back on next daemon startup: `scan_disk` reads the modern
/// workspaces/ toml, and the ADR-0013 migration reads the legacy
/// sessions/ toml. A missing file is success; `false` means a remove or
/// its directory sync failed (logged).
pub(crate) fn remove_row_files(slug: &str) -> bool {
    remove_registration(&[
        crate::workspaces::toml_path_for(slug),
        crate::workspaces::legacy_toml_path_for(slug),
    ])
}

/// Each registration file removed and its directory synced, so the delete
/// survives a power loss: a registration that came back under a record no
/// longer `closing` would resume an ended row (ruling e).
fn remove_registration(paths: &[std::path::PathBuf]) -> bool {
    let mut toml_removed = true;
    for toml_path in paths {
        if let Err(e) = crate::durable::remove(toml_path) {
            tracing::warn!(error = %e, path = ?toml_path, "workspace toml remove failed");
            toml_removed = false;
        }
    }
    toml_removed
}

#[cfg(all(test, unix))]
mod registration_delete_tests {
    use super::*;

    /// A pin, not a power loss (no test can cut the power): a delete whose
    /// directory cannot be synced is not a removed registration.
    #[test]
    fn registration_delete_syncs_its_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("row.toml");
        let mode = |m: u32| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
        std::fs::write(&toml, "x").unwrap();
        // Write and search but no read: the unlink succeeds, and only the
        // directory's open for its sync fails.
        mode(0o300);
        let removed = remove_registration(&[toml.clone(), dir.path().join("absent.toml")]);
        // The shutdown's retry finds the file gone; with its directory still
        // unsynced, that is not a removal either.
        let retried = remove_registration(&[toml.clone()]);
        mode(0o700);
        assert!(!toml.exists(), "the unlink itself failed");
        assert!(!removed, "a registration delete whose directory was not synced counted as removed");
        assert!(!retried, "a retry that found the file gone counted it removed while its directory was still unsynced");
        assert!(remove_registration(&[toml]), "a missing file is success");
    }
}

pub async fn handle_workspace_destroy(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceDestroyReq, WorkspaceDestroyRes};
    let req: WorkspaceDestroyReq =
        serde_json::from_value(payload_json).context("workspace.destroy payload")?;
    tracing::info!(workspace_id = %req.workspace_id, "workspace.destroy");

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        let payload = json!({
            "error": format!("unknown workspace: {}", req.workspace_id),
            "code": "unknown_workspace",
        });
        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    };

    // The default workspace's ROW is never destroyed here — it's the
    // daemon's anchor, with no fallback target to swap ops to. A default
    // TMUX row has no run to end, so it keeps the flat refusal. A
    // default CAPSULE row (ADR 0042: the default `local` row on a
    // Windows FE box, ADR 0043 decision 22: on Linux only ever reached
    // via a hand-edited toml, since the Linux platform default stays
    // "tmux" until the bridge) instead ends its run and keeps the row,
    // reusing the non-default delete's own path below — a `Kept`
    // (unconfirmed) outcome still returns the SAME typed error, never a
    // fabricated success.
    if workspaces.default_id().as_deref() == Some(ws.workspace_id.as_str()) {
        // Gate on the toml's own `runtime` string alone now (ADR 0043
        // decision 22): capsule support is no longer Windows-only, so a
        // Linux default row that genuinely carries `runtime = "capsule"`
        // gets the same real end-run path a Windows one does. Every
        // OTHER default row (the ordinary "tmux" case on every host)
        // keeps the same flat refusal it always had.
        // Same end-run path the non-default delete uses below. The
        // reason is honest for THIS row (not "deleted" — it's kept).
        let (outcome, held_guard) = destroy_capsule_workspace(
            &ws.workspace_id,
            "run ended by the user",
            &ws.agent(),
            &ws.agent_name(),
            &ws.slug,
            &ws.project_root,
            workspaces,
            true,
        )
        .await;
        let (payload, confirmed_ended) =
            default_row_end_response(&ws.workspace_id, &ws.slug, &ws.label, outcome);
        tracing::info!(workspace_id = %ws.workspace_id, confirmed_ended, "workspace.destroy: default row's capsule run outcome; row kept");

        // The row guard (if any) rides along into the reset below and
        // drops only once that returns — see `end_default_row_run`'s own
        // doc.
        end_default_row_run(
            workspaces,
            ws_events,
            &ws.workspace_id,
            &ws.slug,
            &ws.agent_name(),
            confirmed_ended,
            held_guard,
        )
        .await;

        return Ok(vec![(
            Frame::res(req_id, op::WORKSPACE_DESTROY, payload),
            None,
        )]);
    }

    let slug = ws.slug.clone();
    let label = ws.label.clone();
    let workspace_id = ws.workspace_id.clone();
    let agent_name = ws.agent_name();

    // This row's guard, taken below by whichever arm runs — HELD (ADR
    // 0043 decision 33, Codex review round 2; tmux arm added round 3,
    // finding B) across the removal below, past the `if`, so neither a
    // watchdog restart nor a racing `agent.join` can land between a
    // confirmed end and `remove_by_id`. Dropped explicitly once removal
    // is done; stays `None` only for a `Kept` outcome (nothing is
    // removed) or a row that was already gone by the time its arm asked.

    // ADR 0042 slice L1a, Codex review finding 3: a capsule workspace has
    // no tmux session to kill at all — end its run over the supervisor
    // lane instead, and — unlike the tmux kill, which is a best-effort UX
    // nicety — a capsule whose run did NOT reach `record_closed`/
    // `record_verified` STOPS the whole delete here: the row and its
    // toml are kept, and the caller sees a typed error, so a live or
    // unreachable run is never orphaned by a delete that silently
    // "succeeded" out from under it.
    let destroy_guard: Option<tokio::sync::OwnedMutexGuard<()>> = {
        let reason = format!("workspace '{slug}' deleted");
        let (outcome, held) = destroy_capsule_workspace(
            &workspace_id,
            &reason,
            &ws.agent(),
            &agent_name,
            &slug,
            &ws.project_root,
            workspaces,
            true,
        )
        .await;
        match outcome {
            CapsuleDestroyOutcome::Removable(outcome) => {
                tracing::info!(workspace_id = %workspace_id, %outcome, "workspace.destroy: capsule run ended; removing the row");
                held
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                tracing::warn!(workspace_id = %workspace_id, detail = %detail, "workspace.destroy: capsule run not confirmed ended; keeping the row");
                return Ok(vec![(
                    Frame::res(
                        req_id,
                        op::WORKSPACE_DESTROY,
                        capsule_end_not_reached_payload(&detail),
                    ),
                    None,
                )]);
            }
            CapsuleDestroyOutcome::AlreadyRemoved => {
                tracing::info!(workspace_id = %workspace_id, "workspace.destroy: already removed by another end");
                return Ok(vec![(
                    Frame::res(req_id, op::WORKSPACE_DESTROY, capsule_end_not_reached_payload(ALREADY_REMOVED)),
                    None,
                )]);
            }
        }
    };

    // Prune the sot-comm registry. Ending the capsule run takes the agent
    // down before it can run its own comm-leave, so the killer must deregister
    // it — otherwise its row lingers as a ghost in `workspace.list`, which
    // merges the registry (see `resolve_handle`). Drop exactly the rows this
    // workspace owned: by stored `agent_name`, and by the row's own
    // `workspace_id` (covers a manually-joined handle, e.g. `comm-join.sh
    // --name other`, whose `ws.agent_name` was never set). Best-effort +
    // blocking (fs + file lock) → spawn_blocking, non-fatal like the row
    // teardown above.
    let reg_agent = agent_name.clone();
    let reg_ws = workspace_id.clone();
    let reg_host = crate::workspaces::declared_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for destroyed workspace"
        );
    }

    // Best-effort: a remove error is logged + reported but doesn't block
    // the in-memory removal.
    let toml_removed = remove_row_files(&slug);

    // Drop from in-memory registry last. The Arc<Workspace> dropped
    // here is also the one holding the kernel/repl handles; when the
    // last Arc dies their Drop impls run and the Julia children are
    // killed. Other Arc holders (e.g. mid-flight handlers) will keep
    // those processes alive until they finish.
    let _ = workspaces.remove_by_id(&workspace_id);
    // Only now may this row's guard (if any) release — see its own doc
    // above: held from whichever arm took it (capsule end/stop, or the
    // tmux arm's own acquisition, round 3) through this exact removal,
    // so neither a watchdog restart nor a waiting `agent.join` can act
    // on a row that is already gone.
    drop(destroy_guard);

    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll (mirror the create path). Clone
    // because slug/workspace_id are consumed by the bump + response below.
    let _ = ws_events.send(WorkspaceChanged {
        action: "destroyed".into(),
        slug: slug.clone(),
        workspace_id: workspace_id.clone(),
    });

    let rev = session
        .bump(
            "workspace.destroyed",
            json!({ "workspace_id": workspace_id, "slug": slug }),
        )
        .await;

    let res = WorkspaceDestroyRes {
        workspace_id,
        slug,
        label,
        tmux_killed: false,
        toml_removed,
        kept: None,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_DESTROY, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub(crate) use crate::comm::mail::relay::{handle_agent_filed, handle_agent_send};

pub(crate) use crate::comm::registry::join::handle_agent_join;

pub(crate) use crate::comm::mail::filer::{comm_self_host, comm_topology_hub, file_comm, handle_comm_file};

pub(crate) use crate::server::conn::handle_ping;

pub(crate) use crate::comm::registry::registry::{clear_comm_unread, comm_handle_for_workspace, comm_registry_path, host_matches, iso8601_utc_from_secs, iso8601_utc_now, read_comm_agents, read_registry_fresh, remove_comm_agents_for_workspace, unix_now_secs};

pub async fn handle_workspace_list(
    req_id: u64,
    _payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceListEntry, WorkspaceListRes};
    let default_id = workspaces.default_id();
    // Read the sot-comm registry once per list call (fresh — picks up the
    // owning agents' latest `comm-status.sh` writes). `None` when the file is
    // absent/malformed; every lookup below then falls back to empty strings.
    // On a blocking thread: the read's retry sleeps after a failed read.
    let comm_agents = tokio::task::spawn_blocking(read_comm_agents).await.ok().flatten();
    let host = crate::workspaces::declared_host();
    // Pull `.agents[agent_name].<field>` as an owned String, "" if anything is
    // missing or not a string. LU5d2: `agent_name` here is a handle the caller
    // (below) already bound to THIS workspace — by live tmux match or by the
    // stored `agent_name` fallback — never proof it's this host's row, so
    // filter the entry through `host_matches` too: a same-named handle
    // stamped by another host on the shared registry must read as empty, not
    // leak its summary/status_at/state into this host's list.
    let agent_str = |agent_name: &str, field: &str| -> String {
        if agent_name.is_empty() {
            return String::new();
        }
        comm_agents
            .as_ref()
            .and_then(|a| a.get(agent_name))
            .filter(|entry| host_matches(entry, &host))
            .and_then(|entry| entry.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Resolve the comm handle ACTUALLY running in a workspace's tmux session, so
    // manually-joined / pre-state-nav agents (whose `ws.agent_name` was never set
    // — only the spawn path writes it) still bind. `resolve_comm_handle` does the
    // actual matching (shared with `clear_comm_unread` below — one rule, not a
    // copy).
    let ws_list = workspaces.list();
    // Pure memory: kept current by the row's lifecycle observer, no lane query.
    let mut entries: Vec<WorkspaceListEntry> = ws_list
        .into_iter()
        .map(|ws| {
            // Which registry row is this workspace's — one rule, shared with
            // `clear_comm_unread` (`comm_handle_for_workspace`).
            let handle = comm_handle_for_workspace(&ws);
            // The registry `state` IS the badge — no pane scrape, no merge, no
            // precedence to arbitrate. A prior version of this comment described
            // a registry-vs-pane precedence merge that no longer exists here; it
            // was the only thing in the tree suggesting a pane-based idle
            // detector could overrule a stamped fact, which it never can (only
            // an act that could BE the answer clears a question — ADR 0044
            // amendment, field report 2026-09-27).
            let agent_state = agent_str(&handle, "state");
            // `phase` is read straight off the row's own cell — every row
            // is a capsule on this build (`ws.runtime` is always
            // `"capsule"`), so there is no other case left to branch on.
            // A row with no state dir at all reads NEVER_STARTED_PHASE
            // ("stopped") exactly like a row that simply hasn't been
            // attached yet — deliberately NOT distinguished here (Fable
            // review, capsule_workspaces.rs's own "Rule H": a pre-seeded,
            // never-`workspace.create`d row is indistinguishable from a
            // truly orphaned one by any fact this list can cheaply check,
            // and a wire-visible claim otherwise would be dishonest for
            // exactly that row shape). `capsule_workspace::runtime::
            // log_orphaned_state_dirs` still names such rows once at
            // boot, as an operator diagnostic only; `workspace.destroy`'s
            // own real proof (a live lane connect) is what actually
            // decides whether one is removable.
            let state_dir = sot_log::state_dir::sot_state_dir().map(|root| {
                crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id)
                    .to_string_lossy()
                    .into_owned()
            });
            let phase = Some(ws.phase().as_wire_str().to_string());
            WorkspaceListEntry {
                workspace_id: ws.workspace_id.clone(),
                slug: ws.slug.clone(),
                label: ws.label.clone(),
                project_root: ws.project_root.to_string_lossy().into_owned(),
                session_name: ws.session_name.clone(),
                kernel_running: ws.kernel_built(),
                is_default: default_id.as_deref() == Some(ws.workspace_id.as_str()),
                autostart_claude: ws.autostart_claude,
                agent: ws.agent(),
                agent_name: if handle.is_empty() {
                    ws.agent_name()
                } else {
                    handle.clone()
                },
                agent_handle: ws.agent_handle(),
                task: ws.task.clone(),
                agent_state,
                agent_summary: agent_str(&handle, "summary"),
                agent_status_at: agent_str(&handle, "status_at"),
                repl_state: ws.repl_state().to_string(),
                runtime: ws.runtime.clone(),
                state_dir,
                phase,
                activation_error: ws.activation_error(),
                account: ws.account(),
            }
        })
        .collect();
    // Pin the default workspace (the daemon's home anchor) FIRST: the FE never
    // lists it, but its position is the strip's own active-index fallback.
    // Stable sort: every other workspace keeps its alphabetical-by-slug order.
    entries.sort_by(|a, b| b.is_default.cmp(&a.is_default));
    tracing::debug!(count = entries.len(), "workspace.list");
    let res = WorkspaceListRes {
        workspaces: entries,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_LIST, serde_json::to_value(res)?),
        None,
    )])
}

/// `accounts.list` (accounts brief, v0.6.0): discover, fresh, every
/// account this daemon's own home has right now — no declaration to
/// read, no cache. `None` home (unresolvable `$HOME`/`%USERPROFILE%`)
/// answers the empty list rather than an error: nothing to discover is
/// an honest, non-fatal answer, and `workspace.create`'s own account
/// check hits the same "no home" case as its own refusal if it matters
/// there.
pub async fn handle_accounts_list(req_id: u64, _payload_json: serde_json::Value) -> Result<HandlerOutput> {
    use sot_protocol::{AccountEntry, AccountsListRes};
    let accounts = crate::accounts::account_home()
        .map(|home| crate::accounts::discover_accounts(&home))
        .unwrap_or_default();
    let res = AccountsListRes {
        accounts: accounts
            .into_iter()
            .map(|a| AccountEntry {
                name: a.name,
                kinds: a.kinds,
                logged_in: a.logged_in,
            })
            .collect(),
    };
    Ok(vec![(
        Frame::res(req_id, op::ACCOUNTS_LIST, serde_json::to_value(res)?),
        None,
    )])
}

/// `workspace.activate` — builds the ack. The connection-local state this
/// updates (`active_workspace`, `server.rs`'s `handle_connection`) is
/// mutated by the CALLER, not here — this function only resolves
/// `req.workspace_id` (again; the caller does the same resolve to learn
/// what to store, mirroring how the `HELLO` arm computes the auth flag
/// inline before calling `handle_hello`) and echoes back the canonical id,
/// or `None` when it didn't resolve. See `op::WORKSPACE_ACTIVATE` (ops.rs)
/// for the full design.
pub async fn handle_workspace_activate(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceActivateReq, WorkspaceActivateRes};
    let req: WorkspaceActivateReq =
        serde_json::from_value(payload_json).context("workspace.activate payload")?;
    let resolved_ws = workspaces.resolve(req.workspace_id.as_deref());
    let resolved = resolved_ws.as_ref().map(|ws| ws.workspace_id.clone());
    // `read: true` = a PERSON switched the view here (Sessions-Enter,
    // Shift+Left/Right cycling) — clear this row's blue (ADR 0044). The ack
    // below is sent unconditionally, whatever this does or doesn't clear.
    if req.read {
        if let Some(ws) = resolved_ws.clone() {
            let host = crate::workspaces::declared_host();
            let _ = tokio::task::spawn_blocking(move || clear_comm_unread(&ws, &host)).await;
        }
    }
    tracing::info!(
        requested = req.workspace_id.as_deref().unwrap_or("<default>"),
        resolved = resolved.as_deref().unwrap_or("<unresolved>"),
        read = req.read,
        "workspace.activate"
    );
    let res = WorkspaceActivateRes {
        workspace_id: resolved,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_ACTIVATE, serde_json::to_value(res)?),
        None,
    )])
}

#[cfg(test)]
mod duplicate_root_tests {
    use super::find_other_workspace_with_root;
    use crate::workspaces::{Workspace, Workspaces};
    use std::path::{Path, PathBuf};

    /// Unique on-disk dir per test (no tempfile dev-dep; pid + a counter keep
    /// parallel tests from colliding). Never cleaned up — OS temp is fine.
    fn scratch_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "sot-duproot-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).expect("create scratch dir");
        d
    }

    fn ws(label: &str, root: &Path) -> Workspace {
        Workspace::from_label(label, root.to_path_buf(), false, "none".into(), String::new(), String::new())
    }

    fn reg(rows: Vec<Workspace>) -> Workspaces {
        let r = Workspaces::new();
        for w in rows {
            r.insert(w);
        }
        r
    }

    #[test]
    fn same_root_different_slug_is_found() {
        let root = scratch_dir("hit");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("a second identity for one root must be caught");
        assert_eq!(hit.slug, "sot");
    }

    #[test]
    fn same_slug_is_invisible_so_refresh_stays_allowed() {
        // A same-slug create is Workspaces::insert's id-preserving metadata
        // refresh; the gate must not turn that idempotent path into an error.
        let root = scratch_dir("refresh");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "sot", &existing).is_none());
    }

    #[test]
    fn different_roots_pass() {
        let a = scratch_dir("a");
        let b = scratch_dir("b");
        let existing = reg(vec![ws("sot", &a)]);
        let canon = b.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spelling_of_a_registered_root_still_collides() {
        // The incident shape with a twist: the duplicate is registered via a
        // symlink to the same directory. Canonical comparison must see through
        // it — path-string comparison would not.
        let root = scratch_dir("real");
        let link = std::env::temp_dir().join(format!("sot-duproot-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).expect("create symlink");
        let existing = reg(vec![ws("sot", &link)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("symlinked duplicate must be caught");
        assert_eq!(hit.slug, "sot");
    }

    /// ADR 0042 amendment: the inert default anchor (the default row with no
    /// agent, root = the home dir, any runtime) is not a session, so a session
    /// created at that root passes the gate — while a default row that
    /// carries an agent is still refused.
    #[test]
    fn inert_default_anchor_does_not_block_a_session_at_its_root() {
        let root = scratch_dir("anchor");
        let canon = root.canonicalize().unwrap();
        let existing = Workspaces::new();
        let mut anchor = ws("local", &root);
        anchor.runtime = "capsule".to_string();
        let anchor = existing.insert(anchor);
        existing.set_default(&anchor.workspace_id);
        assert!(
            find_other_workspace_with_root(&canon, "home-session", &existing).is_none(),
            "the inert anchor must not claim its root against a real session"
        );
        // Control: the default row WITH an agent is a real session and is
        // still caught (same-slug insert keeps the id, so it stays default).
        let mut sot = ws("local", &root);
        sot.agent = std::sync::Mutex::new("claude".to_string());
        existing.insert(sot);
        assert!(find_other_workspace_with_root(&canon, "home-session", &existing).is_some());
    }

    #[test]
    fn registered_root_that_no_longer_resolves_is_skipped_not_fatal() {
        // A workspace whose root was deleted is Phase 2's (reap) problem; the
        // gate must neither match it nor error on it.
        let gone = std::env::temp_dir().join(format!("sot-duproot-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        let live = scratch_dir("live");
        let existing = reg(vec![ws("dead", &gone)]);
        let canon = live.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }
}

#[cfg(test)]
mod label_in_use_tests {
    use super::same_slug_row_in_use;
    use crate::workspaces::{Observation, Phase, SupervisorIdentity, Workspace, Workspaces};

    fn row(label: &str, runtime: &str, observed: Option<Phase>) -> Workspace {
        let mut w = Workspace::from_label(
            label,
            std::env::temp_dir(),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        w.runtime = runtime.to_string();
        if let Some(phase) = observed {
            assert!(w.apply_phase_observation(Observation::Phase {
                phase,
                supervisor: SupervisorIdentity { pid: 1, created: 1 },
                voyage: Some(uuid::Uuid::from_u128(1)),
            }));
            assert_eq!(w.phase(), phase);
        }
        w
    }

    fn reg(w: Workspace) -> Workspaces {
        let r = Workspaces::new();
        r.insert(w);
        r
    }

    #[test]
    fn stopped_capsule_row_is_not_in_use_so_the_refresh_stays_allowed() {
        let r = reg(row("sot", "capsule", None));
        assert!(same_slug_row_in_use("sot", &r).is_none());
    }

    #[test]
    fn every_observed_phase_is_in_use() {
        for phase in [
            Phase::Starting,
            Phase::Ready,
            Phase::Ending,
            Phase::EndedNoRespawn,
            Phase::Terminal,
        ] {
            let r = reg(row("sot", "capsule", Some(phase)));
            let hit = same_slug_row_in_use("sot", &r)
                .unwrap_or_else(|| panic!("phase {phase:?} must be in use"));
            assert_eq!(hit.slug, "sot");
        }
    }

    #[test]
    fn a_non_capsule_row_is_in_use_even_when_stopped() {
        let r = reg(row("sot", "tmux", None));
        assert!(same_slug_row_in_use("sot", &r).is_some());
    }

    #[test]
    fn a_different_slug_is_not_this_gates_question() {
        let r = reg(row("sot", "capsule", Some(Phase::Ready)));
        assert!(same_slug_row_in_use("other", &r).is_none());
    }
}

#[cfg(test)]
mod workspace_activate_read_tests {
    // End-to-end through the real async handler: `read: true` clears a
    // `done` row via the SAME workspace binding `workspace.list` uses;
    // `read: false` (an old frontend, or any programmatic switch) leaves
    // the registry untouched. The ack echoes the canonical workspace_id
    // regardless of what the clear did.
    use super::*;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_self_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
            match &self.sot_self_host {
                Some(v) => std::env::set_var("SOT_SELF_HOST", v),
                None => std::env::remove_var("SOT_SELF_HOST"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            _serial: serial,
        }
    }

    // A `runtime = "capsule"` row: no tmux pane, no stored `agent_name`
    // (the owner's actual capsule sessions — the ones piling up blue).
    // `agent_handle` seeds `Workspace.agent_handle` directly (ADR 0046
    // decision 1's `agent.join` persistence target) — "" for a row that
    // has never joined.
    fn seed_capsule_workspace(label: &str, agent_handle: &str) -> (Workspaces, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from("/p/x"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        ws.agent_handle = std::sync::Mutex::new(agent_handle.to_string());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        (reg, id)
    }

    async fn activate(
        workspaces: &Workspaces,
        workspace_id: &str,
        read: bool,
    ) -> serde_json::Value {
        let payload = serde_json::json!({ "workspace_id": workspace_id, "read": read });
        let out = handle_workspace_activate(1, payload, workspaces)
            .await
            .expect("handler must not error");
        assert_eq!(
            out.len(),
            1,
            "workspace.activate always answers with exactly one frame"
        );
        out[0].0.payload.clone()
    }

    #[tokio::test]
    async fn capsule_row_read_true_clears_via_declared_agent_handle() {
        // ADR 0046 decision 1: a capsule workspace's row is found ONLY
        // through its DECLARED `agent_handle` (`agent.join`'s persistence
        // target), never through a tmux match, a stored `agent_name`
        // (both empty/absent here), or a daemon-side self-file read-back
        // (deleted) — this is what the owner's actual local capsule
        // sessions look like, so this path clearing is the whole point of
        // the fix.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-workspace-activate-capsule-read-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "host-4");
        let registry_path = dir.join("registry.json");

        let (reg, id) = seed_capsule_workspace("activate-capsule-x", "host-4-activate-capsule-x");

        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "host-4-activate-capsule-x": {
                        "host": "host-4",
                        "state": "done",
                        "done": true,
                        "summary": "capsule probe summary",
                        "status_at": "2026-09-08T00:00:00Z",
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let ack = activate(&reg, &id, true).await;
        assert_eq!(
            ack.get("workspace_id").and_then(|v| v.as_str()),
            Some(id.as_str())
        );
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["host-4-activate-capsule-x"];
        assert_eq!(row["state"], "idle");
        assert!(row.get("done").is_none(), "the done fact must be removed");
        assert_eq!(row["summary"], "capsule probe summary");
        assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod agent_str_host_filter_tests {
    // LU5d2: `handle_workspace_list`'s `agent_str` closure read
    // `.agents[handle].<field>` by handle name alone. The handle it's
    // called with here is bound via the stored `agent_name` fallback (no
    // live tmux row for the session) — a caller-supplied name, not proof
    // of ownership — so a same-named row stamped by ANOTHER host on the
    // shared registry must read as empty, never leak its
    // summary/status_at into this host's `workspace.list`.
    use super::*;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_self_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_SELF_HOST", &self.sot_self_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            _serial: serial,
        }
    }

    #[tokio::test]
    async fn agent_str_never_reads_another_hosts_same_named_row() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-agent-str-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "host-4");
        std::fs::write(
            dir.join("registry.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "same-name": {
                        "host": "hostB",
                        "state": "working",
                        "summary": "leaked",
                        "status_at": "2026-01-01T00:00:00Z"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let workspaces = Workspaces::new();
        let ws = Workspace::from_label(
            "myws",
            std::path::PathBuf::from("/p/myws"),
            false,
            "none".into(),
            "same-name".into(),
            String::new(),
        );
        workspaces.insert(ws);

        let out = handle_workspace_list(1, json!({}), &workspaces)
            .await
            .expect("handler must not error");
        let payload = out[0].0.payload.clone();
        let entries = payload.get("workspaces").unwrap().as_array().unwrap();
        let entry = entries
            .iter()
            .find(|e| e.get("slug").and_then(|v| v.as_str()) == Some("myws"))
            .expect("the workspace we inserted must be in the list");
        assert_eq!(
            entry.get("agent_summary").and_then(|v| v.as_str()),
            Some("")
        );
        assert_eq!(
            entry.get("agent_status_at").and_then(|v| v.as_str()),
            Some("")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod workspace_destroy_default_row_tests {
    // Default-row end-run: a default TMUX row keeps the flat refusal
    // (no run to end). A default CAPSULE row (ADR 0043 decision 22: on
    // any host the capsule runtime compiles for, not just Windows) ends
    // its run and keeps the row, reporting the outcome in
    // `WorkspaceDestroyRes::kept` — but only when CONFIRMED
    // (`Removable`); `Kept` still returns the typed
    // `capsule_end_not_reached` error, never the flat tmux-style refusal.
    use super::*;

    // Isolates `crate::workspaces::save`'s config dir -- `pin_local_
    // state_root` below now pins `XDG_CONFIG_HOME` unconditionally
    // alongside state, so this is no longer "the one test below" that
    // reaches the reset+persist path for real: an `Orphaned` outcome
    // (this fix) can make ANY of them reach a real toml write or removal,
    // and every one now runs through the same scratch config root.
    // Same technique as `workspaces.rs`'s own `env_guarded`, serialized
    // under the crate-wide lock so this never races another module's
    // env-mutating test. Ungated since the macOS wiring lane: the
    // absence proof `seed_provably_unheld_state_dir` builds means
    // something on every host this daemon builds for, so this cluster
    // has real callers everywhere and needs no dead-code suppression.
    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_config_home: Option<std::ffi::OsString>,
        // Added alongside `seed_provably_unheld_state_dir` below (ADR
        // 0043 decision 33, Codex review, 2026-09-11): the tests that used
        // to lean on `mark_capsule_terminal`'s now-deleted unguarded fast
        // path instead point `sot_log::state_dir::sot_state_dir()` at a
        // scratch root so `destroy_capsule_workspace`'s real guarded path
        // finds a hermetic, provably-absent state dir there. Both vars are
        // saved/restored on every platform even though `sot_state_dir()`
        // only ever reads ONE of them per platform (`XDG_STATE_HOME` on
        // Unix, `LOCALAPPDATA` on Windows — see `pin_local_state_root`
        // below): a fixture that pinned only `XDG_STATE_HOME` used to be
        // silently ignored by the resolver on Windows CI, which is exactly
        // how the terminal/confirmed-end tests below used to fail there —
        // the fixture built a state dir nobody ever looked at.
        xdg_state_home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        sot_self_host: Option<std::ffi::OsString>,
        sot_comm_home: Option<std::ffi::OsString>,
        // Added for the real-listener refusal proof below (`sot_log::
        // state_dir::runtime_dir` trusts this once it is absolute and
        // private) -- captured/restored exactly like the other five so a
        // test that pins it never leaks the override past its own scope.
        sot_runtime_dir: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("XDG_STATE_HOME", &self.xdg_state_home),
                ("LOCALAPPDATA", &self.localappdata),
                ("SOT_SELF_HOST", &self.sot_self_host),
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_RUNTIME_DIR", &self.sot_runtime_dir),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn env_guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_runtime_dir: std::env::var_os("SOT_RUNTIME_DIR"),
            _serial: serial,
        }
    }

    /// Points wherever `sot_log::state_dir::sot_state_dir()` ACTUALLY reads
    /// on this platform (`LOCALAPPDATA` on Windows, `XDG_STATE_HOME`
    /// elsewhere — that function's own doc has the precedence) at `dir`,
    /// then returns the root by calling that SAME resolver rather than
    /// hand-building `dir.join("sot")` here — the one seam every fixture
    /// below must agree with `destroy_capsule_workspace` about. `dir`
    /// need not exist yet — nothing here creates it; `state_dir_missing`
    /// fixtures rely on exactly that. The caller must hold an `EnvGuard`
    /// (`env_guarded()`) FIRST, captured before this call touches
    /// anything — its `Drop` puts `XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
    /// `LOCALAPPDATA`, `SOT_SELF_HOST` and `SOT_COMM_HOME` back to
    /// whatever `env_guarded()` observed at that moment, which is every
    /// var this function or any of its callers may set — but that
    /// restore is only as good as the crate-wide serialization every one
    /// of these fixtures shares (`paths::ENV_TEST_LOCK`): it is NOT a
    /// claim that `XDG_STATE_HOME` (or any of the five) is protected
    /// from a test elsewhere in this binary that mutates it without
    /// taking the same lock.
    ///
    /// Also isolates `XDG_CONFIG_HOME` on every non-Windows platform (the
    /// field defect this closes): `sot_config_dir()` reads a SEPARATE var
    /// there, entirely independent of `XDG_STATE_HOME`
    /// (`state_dir.rs`'s own doc — only Windows derives config from the
    /// same `LOCALAPPDATA` root), so pinning state alone left config free
    /// to resolve to the real `$HOME/.config/sot` the instant any test
    /// through this fixture reached `crate::workspaces::save` or a toml
    /// removal — exactly the shape of the leaked row this whole fix
    /// exists to close: a scratch daemon whose state root was isolated
    /// but whose config directory was not, writing (and then, once a row
    /// with no state dir can be proven `Orphaned`, DELETING) a real
    /// per-host workspace registry entry. Pinned to the SAME `dir` as
    /// state (Fable review: no separate sibling path to invent or keep
    /// in sync) — `sot_state_dir()` and `sot_config_dir()` both append
    /// their own distinct subtree name under it (`state_dir.rs`'s own
    /// doc), so the two never collide even sharing one root; "nothing
    /// exists under `dir`" fixtures stay literally true either way.
    fn pin_local_state_root(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        std::env::set_var("LOCALAPPDATA", dir);
        #[cfg(not(windows))]
        {
            std::env::set_var("XDG_STATE_HOME", dir);
            std::env::set_var("XDG_CONFIG_HOME", dir);
        }
        sot_log::state_dir::sot_state_dir()
            .expect("state root must resolve once pinned to a scratch dir")
    }

    /// Builds a hermetic on-disk state dir that `destroy_capsule_
    /// workspace`'s real guarded path (ADR 0043 decision 33) will
    /// independently prove BOTH halves of the destroy proof absent for —
    /// nothing ever holds `supervisor.lock`, and a published pointer
    /// names a voyage whose own `writer.lock` exists and is free — so
    /// `end_run`'s `Unheld` arm reports `Removable` with no live process
    /// anywhere. Caller must first call `pin_local_state_root` (under
    /// `env_guarded`) and pass ITS return value as `state_root` — the
    /// resolved root `sot_log::state_dir::sot_state_dir()` itself reports,
    /// never a hand-built path, so this fixture lands exactly where
    /// `destroy_capsule_workspace` (via `state_dir_for`) actually looks.
    /// Replaces this module's old reliance on `mark_capsule_terminal`'s
    /// deleted unguarded fast path (Codex review, 2026-09-11: that path
    /// returned `Removable` on the daemon's own say-so alone, with no
    /// proof at all) — same technique `capsule_workspace`'s own
    /// absence-proof unit tests use. Ungated since the macOS wiring
    /// lane: the body reaches `capsule_workspace::runtime`, which no
    /// longer carries a platform gate at its own root, so this fixture
    /// exists wherever the daemon does.
    fn seed_provably_unheld_state_dir(state_root: &std::path::Path, workspace_id: &str) {
        let state_dir = crate::capsule_workspace::state_dir_for(state_root, workspace_id);
        std::fs::create_dir_all(&state_dir).expect("create the fake state dir");
        let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
        sot_log::pointer::publish(&state_dir, voyage_id).expect("publish the pointer");
        let voyage_root = sot_log::supervisor::voyage_root_path(&state_dir, voyage_id);
        std::fs::create_dir_all(&voyage_root).expect("voyage root");
        std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
    }

    fn seed_default(runtime: &str) -> (Workspaces, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "local",
            std::path::PathBuf::from("/p/local"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        ws.runtime = runtime.to_string();
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        (reg, id)
    }

    /// Same as `seed_default("capsule")` but with a carried-over agent —
    /// the field shape (owner once started an agent in this row before
    /// the "nothing runs in the anchor" rule existed) that the reset in
    /// `end_default_row_run` exists to unstick.
    fn seed_default_with_agent(agent: &str, agent_name: &str) -> (Workspaces, String, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "local",
            std::path::PathBuf::from("/p/local"),
            true,
            agent.to_string(),
            agent_name.to_string(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        let id = ws.workspace_id.clone();
        let slug = ws.slug.clone();
        reg.insert(ws);
        reg.set_default(&id);
        (reg, id, slug)
    }

    async fn destroy(workspaces: &Workspaces, workspace_id: &str) -> serde_json::Value {
        let session = Session::new();
        let (tx, _rx) = broadcast::channel(16);
        let payload = json!({ "workspace_id": workspace_id });
        let out = handle_workspace_destroy(1, payload, &session, workspaces, &tx)
            .await
            .expect("handler must not error");
        assert_eq!(
            out.len(),
            1,
            "workspace.destroy always answers with exactly one frame"
        );
        out[0].0.payload.clone()
    }

    // ADR 0043 decision 22: capsule support is no longer Windows-only, so
    // a default row explicitly marked "capsule" (a hand-edited toml, or
    // later the bridge) now takes the SAME real end-run path a Windows
    // one always did — never the flat tmux-style refusal
    // (`default_workspace_not_destroyable`). Nothing is actually running
    // behind this row in-process, so the real attempt cannot reach a
    // live lane. The exact outcome is platform-dependent: on Windows and
    // Linux, `destroy_capsule_workspace`'s real path finds no state dir
    // at all on disk for this synthetic, never-spawned row AND
    // `query_status`'s own connect fails with decision 27's "no listener
    // at all" shape (nothing was ever bound at this row's lane address
    // either) — the orphan-removal fix this test now covers: proven,
    // not merely refused, so the row's run is confirmed ended
    // (`orphan_removed`) exactly as a real end would be, never the
    // flat refusal AND never a bare `Kept`. One outcome on every host
    // since the macOS wiring lane: there is no platform-shaped fallback
    // arm left for this to mean something different on.
    // Pinned hermetic (Codex review, 2026-09-11): this test used to read
    // `sot_log::state_dir::sot_state_dir()`'s REAL, unpinned environment —
    // fine on a dev box whose shell always exports a stable, qualified
    // `XDG_STATE_HOME`, but on CI (nothing exported) it read whatever the
    // ambient state root happened to resolve to, unguarded against every
    // OTHER test in this module that mutates the SAME process-global vars
    // under `env_guarded()`'s lock. Pinning to a fresh, never-created
    // scratch root — same resolver, same lock — makes "no state dir on
    // disk for this workspace" true by construction, not by luck.
    // `pin_local_state_root` now also isolates `XDG_CONFIG_HOME` (the
    // harness fix this same effort closes): once this scenario proves
    // `Orphaned` instead of merely refusing, the response path really
    // does reach `crate::workspaces::save`'s reset-persist write, which
    // must never land under a real `~/.config/sot`.
    #[tokio::test]
    async fn default_capsule_workspace_with_no_state_dir_is_proven_orphaned_not_a_flat_refusal() {
        let _guard = env_guarded();
        let scratch = std::env::temp_dir().join(format!(
            "sot-ws-destroy-missing-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The resolved STATE ROOT is created (Fable review, safety: the
        // orphan proof now refuses outright unless the root itself
        // canonicalizes -- `destroy_capsule_workspace`'s own doc) but
        // nothing under it is: the point of this test is that THIS ROW's
        // own state dir does not exist on disk at all, and nothing was
        // ever bound at its lane address either.
        {
            let root = pin_local_state_root(&scratch);
            std::fs::create_dir_all(&root).unwrap();
        }

        let (reg, id) = seed_default("capsule");
        let payload = destroy(&reg, &id).await;
        assert_ne!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("default_workspace_not_destroyable"),
            "a capsule default row must not get the flat tmux-style refusal: {payload:?}"
        );
        {
            assert_eq!(
                payload.get("code").and_then(|v| v.as_str()),
                None,
                "a proven orphan is a CONFIRMED end, not a `Kept` error: {payload:?}"
            );
            let kept = payload.get("kept").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                kept.contains("orphan_removed"),
                "the orphan proof's own distinct outcome must be visible: {payload:?}"
            );
        }
        assert!(reg.resolve(Some(&id)).is_some(), "the default row is never removed either way");

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The refusing half of the same proof, for real (Fable review, item
    /// 6): a row with no state dir whose lane IS actually answered by
    /// something — a bare listener, bound at exactly this row's own
    /// `supervisor-<h>.sock`, that never speaks the protocol back — must
    /// never be classified `is_definitely_orphaned`. `connect(2)` itself
    /// succeeds against a bound-and-listening socket even with nothing
    /// ever `accept`ing it, so `query_status`'s connect step does NOT
    /// return decision 27's absent shape (`ENOENT`/`ECONNREFUSED`); it
    /// times out instead, waiting on a hello nobody answers — exactly the
    /// "something is there, but unresponsive" case that must keep
    /// refusing. `SOT_RUNTIME_DIR` is pinned to a fresh, private (owner-
    /// only) scratch dir so the real socket path (`sot_log::socket_unix::
    /// supervisor_socket_path`) never collides with a real session.
    /// `cfg(unix)`: the assertion target is `capsule_workspace::runtime::
    /// is_definitely_orphaned`'s refusing half, and `mod runtime` lost
    /// its platform gate in the macOS wiring lane — exactly the change
    /// this gate's predecessor said it would widen with. The socket half
    /// was never the constraint (`sot_log::socket_unix` and
    /// `supervisor_client` both compile for Darwin).
    #[tokio::test]
    #[cfg(unix)]
    async fn a_reachable_listener_with_no_state_dir_still_refuses() {
        let _guard = env_guarded();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        let scratch = std::env::temp_dir().join(format!("sot-ws-destroy-listener-test-state-{stamp}"));
        let root = pin_local_state_root(&scratch);
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = root.canonicalize().expect("the scratch root was just created");

        // NOT `temp_dir()`: a unix socket path is capped at `sun_path`
        // (104 bytes on macOS, 108 on Linux) and macOS's temp dir is a
        // deep `/var/folders/<..>/<..>/T/` path, so the supervisor socket
        // built under it overflows and this test dies `PathTooLong` before
        // it can assert anything. `/tmp` is also what `runtime_sot_dir`
        // itself falls back to when no private runtime dir exists, which
        // is the production shape on macOS -- so this keeps the test on
        // the same path length the real thing gets. Kept short for the
        // same reason: the name below plus `supervisor-<16 hex>.sock`
        // must still fit.
        let runtime_dir = std::path::PathBuf::from("/tmp").join(format!("sot-wsdl-rt-{stamp}"));
        std::fs::create_dir_all(&runtime_dir).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::env::set_var("SOT_RUNTIME_DIR", &runtime_dir);

        let (reg, id) = seed_default("capsule");
        // The EXACT path `destroy_capsule_workspace` will dial: the same
        // canonical-root-then-join this fix's own destroy site uses, fed
        // to the SAME hash the production lane address is built from.
        let state_dir = crate::capsule_workspace::state_dir_for(&canonical_root, &id);
        let h = sot_log::state_dir::state_dir_hash(&state_dir);
        let sock_path =
            sot_log::socket_unix::supervisor_socket_path(&h).expect("runtime dir was just pinned");
        let _listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .unwrap_or_else(|e| panic!("bind the stand-in listener at {sock_path:?}: {e}"));
        // Never `accept()`s -- proves a merely-unresponsive lane, not an
        // absent one.

        let payload = destroy(&reg, &id).await;
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("state_dir_missing"),
            "a lane that answers (even silently) must never be treated as orphaned: {payload:?}"
        );
        assert!(reg.resolve(Some(&id)).is_some(), "a refused destroy never removes the row");
        assert!(!state_dir.exists(), "destroy on a missing state dir must never recreate it");

        let _ = std::fs::remove_dir_all(&scratch);
        let _ = std::fs::remove_dir_all(&runtime_dir);
    }

    // ADR 0043 decision 33 (BLOCKER, Codex review, 2026-09-11): a row the
    // watchdog already marked `capsule_terminal` no longer takes an
    // unguarded shortcut straight to `Removable` -- that deleted fast
    // path returned "removable" on the daemon's own say-so alone,
    // bypassing the guard AND the fence/leg absence proof, so a leg the
    // watchdog's own exhausted restart budget (or a failed adoption) left
    // running behind a `Terminal` authority could have been orphaned. A
    // terminal row now goes through the SAME guarded resume/end_run path
    // as every other row: `resume_locked`'s own internal `is_capsule_
    // terminal` check still reports that phase without a live round trip
    // (no wasted probe against an authority that is almost always
    // already gone), but `end_run`'s fresh `query_status` -- naturally
    // unreachable here, nothing is listening -- then reaches the SAME
    // independent absence proof every other row does, hermetically
    // reproduced via `seed_provably_unheld_state_dir`. Called directly
    // (not through `handle_workspace_destroy`) to stay hermetic -- the
    // full wire path also removes on-disk tomls under the real config
    // dir, which is not safe to exercise from an in-process unit test.
    //
    // Gated (unlike the deleted portable shortcut this replaces): the
    // absence proof this now exercises lives entirely inside
    // `destroy_capsule_workspace`'s `#[cfg(any(windows, target_os =
    // "linux"))]` arm -- every other host takes the unconditional `Kept`
    // fallback regardless of any on-disk fixture.
    #[tokio::test]
    async fn a_capsule_workspace_marked_terminal_still_needs_the_absence_proof() {
        let _guard = env_guarded();
        let scratch = std::env::temp_dir().join(format!(
            "sot-ws-destroy-terminal-proof-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state_root = pin_local_state_root(&scratch);

        let (reg, id) = seed_default("capsule");
        // An epoch must begin before an observation about it is accepted,
        // so seed one before forcing the phase cell to Terminal.
        let ws = reg.resolve(Some(id.as_str())).expect("row just seeded");
        let identity = crate::workspaces::SupervisorIdentity { pid: 1, created: 1 };
        ws.begin_supervisor_epoch(identity);
        ws.apply_phase_observation(crate::workspaces::Observation::Phase {
            phase: crate::workspaces::Phase::Terminal,
            supervisor: identity,
            voyage: None,
        });
        seed_provably_unheld_state_dir(&state_root, &id);

        // `/p/local`/`"local"` are placeholders (`resume_locked`'s own
        // `Phase::Terminal` check returns before any agent argv is
        // resolved) -- only `state_root`'s on-disk fixture is real.
        let (outcome, held) = destroy_capsule_workspace(
            &id,
            "test reason",
            "none",
            "",
            "local",
            std::path::Path::new("/p/local"),
            &reg,
            true,
        )
        .await;
        // The guard IS taken now (Codex review: the deleted fast path's
        // `None` bypassed it) -- dropped once this proof has run.
        assert!(held.is_some(), "a terminal row must take the same row guard every other row does");
        match outcome {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(_) => {}
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!(
                    "a terminal row with both halves of the absence proof independently absent \
                     must be Removable: {detail}"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&scratch);
    }

    // The full field defect this lane fixes: a default row carrying an
    // agent from before the anchor rule, whose run is CONFIRMED ended,
    // must have its `agent`/`agent_name` reset to the inert-anchor
    // shape, that reset persisted to its toml, and the existing
    // `run_ended` broadcast still fired -- all through the real
    // `handle_workspace_destroy` wire path. Hermetic despite going
    // through the full handler: `seed_provably_unheld_state_dir` (ADR
    // 0043 decision 33's own absence proof, reproduced on disk -- the
    // technique the test above also uses) makes the outcome
    // deterministic with no live supervisor at all, and
    // `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`SOT_SELF_HOST` are pinned to a
    // scratch dir so neither the toml write nor the state dir ever
    // touches a real `~/.config/sot` or `~/.local/state/sot`.
    //
    // The absence proof `seed_provably_unheld_state_dir` targets is
    // `destroy_capsule_workspace`'s one, ungated path (macOS wiring
    // lane), so this runs on every host.
    #[tokio::test]
    async fn default_row_confirmed_ended_resets_agent_persists_toml_and_broadcasts() {
        let _guard = env_guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-ws-destroy-default-reset-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "reset-test-host");
        let state_root = pin_local_state_root(&dir.join("state"));

        let (reg, id, slug) = seed_default_with_agent("claude", "kal-local");
        assert!(
            !reg.is_inert_default_anchor(&reg.resolve(Some(&id)).unwrap()),
            "a default row carrying an agent is a real session, not the anchor, before the fix runs"
        );
        seed_provably_unheld_state_dir(&state_root, &id);

        let session = Session::new();
        let (tx, mut rx) = broadcast::channel(16);
        let payload = json!({ "workspace_id": id });
        let out = handle_workspace_destroy(1, payload, &session, &reg, &tx)
            .await
            .expect("handler must not error");
        let response = out[0].0.payload.clone();
        assert!(
            response.get("error").is_none(),
            "a confirmed end must not error: {response:?}"
        );
        assert!(
            response.get("kept").is_some(),
            "a confirmed end reports the success shape: {response:?}"
        );

        // The row: agent reset, inert again, id unchanged.
        let after = reg
            .resolve(Some(&id))
            .expect("the default row is never removed");
        assert_eq!(after.workspace_id, id);
        assert_eq!(after.agent(), "none");
        assert_eq!(after.agent_name(), "");
        assert!(
            reg.is_inert_default_anchor(&after),
            "with agent reset to none, the default row must be inert again"
        );

        // The broadcast: the existing `run_ended` WorkspaceChanged, unchanged.
        let evt = rx
            .try_recv()
            .expect("run_ended must still be broadcast on a confirmed end");
        assert_eq!(evt.action, "run_ended");
        assert_eq!(evt.workspace_id, id);
        assert_eq!(evt.slug, slug);

        // The toml: the reset was persisted, not just held in memory.
        let toml_path = crate::workspaces::toml_path_for(&slug);
        let contents = std::fs::read_to_string(&toml_path)
            .unwrap_or_else(|e| panic!("toml must be persisted at {toml_path:?}: {e}"));
        assert!(
            contents.contains("agent         = \"none\""),
            "agent must persist as none:\n{contents}"
        );
        assert!(
            contents.contains("agent_name    = \"\""),
            "agent_name must persist as empty:\n{contents}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ADR 0043 decision 35: the default row's end prunes its sot-comm
    // registry row the same way `workspace.destroy`'s non-default path
    // already does (`remove_comm_agents_for_workspace_host_tests` proves
    // that path in isolation) -- before this lane, only the destroy path
    // pruned, so a Windows default-row end left a ghost row that
    // `workspace.list` merged back in. Two rows share the agent's handle
    // string on the shared registry, one on this test's host and one on
    // another, to prove the prune is host-scoped exactly like the
    // destroy-path prune it mirrors.
    //
    // Same reason as the reset test above -- the confirmed-end outcome
    // `seed_provably_unheld_state_dir` produces reaches `Removable`
    // through `destroy_capsule_workspace`'s one, ungated path.
    #[tokio::test]
    async fn default_row_end_prunes_the_rows_registry_row() {
        let _guard = env_guarded();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let config_dir =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-cfg-{stamp}"));
        let comm_dir =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-comm-{stamp}"));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&comm_dir).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
        std::env::set_var("SOT_COMM_HOME", &comm_dir);
        std::env::set_var("SOT_SELF_HOST", "leave-test-host");
        let scratch_state =
            std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-state-{stamp}"));
        let state_root = pin_local_state_root(&scratch_state);

        let handle = "default-row-leave-handle";
        std::fs::write(
            comm_dir.join("registry.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    handle: {"host": "leave-test-host"},
                    "other-host-handle": {"host": "another-host"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (reg, id, _slug) = seed_default_with_agent("claude", handle);
        seed_provably_unheld_state_dir(&state_root, &id);
        let payload = destroy(&reg, &id).await;
        assert!(
            payload.get("error").is_none(),
            "a confirmed end must not error: {payload:?}"
        );

        let after: serde_json::Value = serde_json::from_slice(
            &std::fs::read(comm_dir.join("registry.json")).unwrap(),
        )
        .unwrap();
        let agents = after.get("agents").unwrap().as_object().unwrap();
        assert!(
            !agents.contains_key(handle),
            "the default row's own handle must be pruned on a confirmed end: {agents:?}"
        );
        assert!(
            agents.contains_key("other-host-handle"),
            "another host's same-named-session row must survive: {agents:?}"
        );

        let _ = std::fs::remove_dir_all(&config_dir);
        let _ = std::fs::remove_dir_all(&comm_dir);
        let _ = std::fs::remove_dir_all(&scratch_state);
    }

    // A lane still `Starting` is never "not running" -- retryable
    // `Kept`, never a fabricated "was not running" success.
    #[test]
    fn starting_outcome_maps_to_a_retryable_kept_not_not_running() {
        let outcome = capsule_destroy_outcome_of(crate::capsule_workspace::EndRunOutcome::Starting);
        match outcome {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Kept { detail } => {
                assert_eq!(detail, "supervisor is starting; retry");
            }
            CapsuleDestroyOutcome::Removable(detail) => {
                panic!("Starting must never be reported Removable (\"ended\"): {detail}");
            }
        }
    }

    // An authority found ALREADY resting in `EndedNoRespawn` is
    // `AlreadyEnded`, not a fabricated `RecordVerified` -- still
    // `Removable` (safe to report "ended").
    #[test]
    fn already_ended_outcome_is_removable_and_distinct_from_record_verified() {
        let outcome =
            capsule_destroy_outcome_of(crate::capsule_workspace::EndRunOutcome::AlreadyEnded);
        match outcome {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    !detail.contains("verified"),
                    "must not claim verification it never observed: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("AlreadyEnded is a confirmed end — must not be Kept: {detail}");
            }
        }
    }

    // Rounds out coverage of `capsule_destroy_outcome_of`'s remaining
    // variants: a real end_run's own two confirmed outcomes both map to
    // `Removable`, and `NotEnded` (failed/refused/outcome-unknown) maps
    // to `Kept`.
    #[test]
    fn record_verified_and_closed_are_removable_not_ended_is_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        for outcome in [O::RecordVerified, O::RecordClosed] {
            assert!(
                matches!(
                    capsule_destroy_outcome_of(outcome.clone()),
                    CapsuleDestroyOutcome::Removable(_)
                ),
                "{outcome:?} must map to Removable"
            );
        }
        match capsule_destroy_outcome_of(O::NotEnded("end_run failed: boom".to_string())) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Kept { detail } => assert_eq!(detail, "end_run failed: boom"),
            CapsuleDestroyOutcome::Removable(detail) => {
                panic!("NotEnded must never map to Removable: {detail}");
            }
        }
    }

    // A leg that went `Terminal` (e.g. an unlaunchable agent argv) has no
    // live run to orphan — `end_run` already sent it `stop` and waited
    // for confirmed exit before ever reporting this outcome, so the row
    // must be `Removable`, never stuck `Kept` forever (the gap this
    // whole variant closes: an unendable capsule row).
    #[test]
    fn terminal_outcome_is_removable_not_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Terminal) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    detail.contains("terminal"),
                    "detail should explain the row was terminal: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Terminal is a confirmed end (stop was sent and awaited) — must not be Kept: {detail}");
            }
        }
    }

    // `Unheld` (no supervisor holds the row — see its own doc) is a
    // confirmed end, same family as `Terminal`/`AlreadyEnded`: `Removable`,
    // never `Kept`.
    #[test]
    fn unheld_outcome_is_removable_not_kept() {
        use crate::capsule_workspace::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Unheld) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert_eq!(detail, "no supervisor held the row");
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Unheld means nobody holds the row — must not be Kept: {detail}");
            }
        }
    }

    // A `Kept` outcome must build the SAME typed error the non-default
    // path returns, and must NEVER signal a `run_ended` broadcast.
    #[test]
    fn kept_outcome_builds_the_typed_error_and_never_broadcasts_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Kept {
                detail: "supervisor is starting; retry".to_string(),
            },
        );
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("capsule_end_not_reached")
        );
        assert!(payload.get("error").is_some());
        assert!(
            payload.get("workspace_id").is_none(),
            "must not carry the success shape's own fields: {payload:?}"
        );
        assert!(payload.get("kept").is_none());
        assert!(
            !confirmed_ended,
            "a Kept outcome must never signal a run_ended broadcast"
        );
    }

    // The mirror case: a `Removable` (confirmed) outcome DOES build the
    // success shape and DOES signal the broadcast.
    #[test]
    fn removable_outcome_builds_success_and_signals_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        );
        assert!(
            payload.get("error").is_none(),
            "must not error: {payload:?}"
        );
        assert_eq!(
            payload.get("workspace_id").and_then(|v| v.as_str()),
            Some("ws-local-1")
        );
        assert!(payload.get("kept").and_then(|v| v.as_str()).is_some());
        assert!(confirmed_ended);
    }
}
