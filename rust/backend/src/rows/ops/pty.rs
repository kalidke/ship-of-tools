//! pty.input and pty.screen: type into, or read the screen of, a capsule row through its supervisor lane.

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
use crate::handlers::HandlerOutput;
use crate::workspaces::Workspaces;

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
/// row (a WATCHER attach).
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
