//! pty.open, pty.input and pty.screen: open a capsule row's pane, type into it, or read its screen, through its supervisor lane.
//! Also the test-only activation barrier and marker that `pty.open`'s start-on-attach runs under.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use sot_protocol::PtyCursor;
use sot_protocol::PtyEnter;
use sot_protocol::PtyInputReq;
use sot_protocol::PtyInputRes;
use sot_protocol::PtyOpenReq;
use sot_protocol::PtyScreenReq;
use sot_protocol::PtyScreenRes;
use crate::paths;
use crate::server::reply::HandlerOutput;
use crate::server::reply::write_frame_to;
use tokio::io::AsyncWrite;
use crate::rows::Workspace;
use crate::rows::Workspaces;

/// `PtyInputReq::origin` / `PtyScreenReq` share no size limit of their own
/// — this one is `origin`'s: ADR 0042 amendment §1, "≤128 bytes, else
/// `bad_origin`."
const MAX_PTY_INPUT_ORIGIN_LEN: usize = 128;

/// One capsule-lane op's absolute deadline (ADR 0042 amendment: "the whole
/// operation runs under ONE deadline (5 s): attach, checkpoint, take,
/// input, ack, detach"). Shared by both `pty.input` and `pty.screen`'s
/// capsule arms, which are the only readers.
const CAPSULE_OP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Bounds for [`crate::rows::run::headless::write_and_enter`]'s
/// pacing wait — never a confirmation, only pacing.
const CAPSULE_WRITE_QUIET_BUDGET: std::time::Duration = std::time::Duration::from_millis(300);
const CAPSULE_WRITE_PACING_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// What a capsule-runtime `pty.input`/`pty.screen` op's `spawn_blocking`
/// closure reports — computed OFF the async runtime (the phase probe and
/// the headless client both make blocking IPC calls), then translated to a
/// response frame back on the async side. Ungated, like
/// `rows::run::headless` itself (macOS wiring lane): one variant
/// set, one outcome shape, on every host this daemon builds for.
enum CapsuleOpOutcome<T> {
    Ok(T),
    NotReady(&'static str),
    Headless(crate::rows::run::headless::HeadlessError),
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
    e: crate::rows::run::headless::HeadlessError,
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
#[allow(clippy::too_many_lines, reason = "the pty.input handler: attribution, resume and the write to the row's lane; predates the 100-line limit")]
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
                let Some(state_root) = sot_log::host::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::rows::spawn::state_root::STATE_ROOT_HINT
                        ),
                        "code": "capsule_input_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_INPUT, payload), None)]);
                };
                let state_dir =
                    crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id);
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
                    let phase = match crate::rows::run::activation::resume_if_absent(
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
                            crate::rows::run::probe::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::rows::run::probe::phase_str(sot_log::lane::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    // Split write+pace+enter: `write_and_enter`'s own doc.
                    if enter {
                        match crate::rows::run::headless::write_and_enter(
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
                        match crate::rows::run::headless::type_into(&state_dir, &controller_id, &bytes, deadline) {
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
#[allow(clippy::too_many_lines, reason = "the pty.screen handler: finds the row and reads its current screen; predates the 100-line limit")]
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
                let Some(state_root) = sot_log::host::state_dir::sot_state_dir() else {
                    let payload = json!({
                        "error": format!(
                            "could not resolve this machine's state root ({} unset)",
                            crate::rows::spawn::state_root::STATE_ROOT_HINT
                        ),
                        "code": "capsule_screen_failed",
                        "phase": "attach",
                    });
                    return Ok(vec![(Frame::res(req_id, op::PTY_SCREEN, payload), None)]);
                };
                let state_dir =
                    crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id);
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
                    let phase = match crate::rows::run::activation::resume_if_absent(
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
                            crate::rows::run::probe::UNREACHABLE_PHASE
                        }
                    };
                    let ready_phase =
                        crate::rows::run::probe::phase_str(sot_log::lane::wire::SupervisorPhase::Ready);
                    if phase != ready_phase {
                        return CapsuleOpOutcome::NotReady(phase);
                    }
                    let deadline = std::time::Instant::now() + CAPSULE_OP_DEADLINE;
                    match crate::rows::run::headless::screen_of(
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

/// Answers `pty.open`: refuses a bad payload or target, starts the row's supervisor on attach and replies `attach_direct`.
pub(crate) async fn handle_pty_open<W>(tx: &mut W, frame: Frame, workspaces: &Workspaces) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let req: PtyOpenReq = match serde_json::from_value(frame.payload) {
        Ok(r) => r,
        Err(e) => {
            let payload = serde_json::json!({
                "error": format!("pty.open payload: {e}"),
                "code": "bad_request",
            });
            write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                .await?;
            return Ok(());
        }
    };
    // Name validation (security review).
    if let Some(t) = req.target.as_deref() {
        if !paths::valid_name(t) {
            let payload = serde_json::json!({
                "error": format!(
                    "invalid target {t:?} (want 1-64 chars of [A-Za-z0-9._-])"
                ),
                "code": "bad_target",
            });
            write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                .await?;
            return Ok(());
        }
    }
    let requested_target = req
        .target
        .as_deref()
        .unwrap_or("");
    // A row's agent pane is a capsule (ADR 0046): `pty.open`
    // starts its supervisor when needed and answers
    // `attach_direct` with the `state_dir` the frontend attaches
    // to (L1b, the U3 client). An unknown target has nothing
    // to attach to.
    let Some(ws) = workspaces.workspace_for_tmux(requested_target) else {
        let payload = serde_json::json!({
            "error": format!("no workspace owns session {requested_target:?}"),
            "code": "no_workspace",
        });
        write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
            .await?;
        return Ok(());
    };
    let state_root = sot_log::host::state_dir::sot_state_dir();
    // `attach_direct` answers at once from memory, no
    // lane probe here -- `ensure_started` runs
    // fire-and-forget in the background under its own
    // guard, so a stale cached `Ready` never blocks it.
    {
        start_on_attach(&ws, workspaces, &state_root);
    }
    let state_dir = state_root
        .map(|root| crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id))
        .map(|p| p.to_string_lossy().into_owned());
    let payload = serde_json::json!({
        "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
        "code": "attach_direct",
        "state_dir": state_dir,
    });
    write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
        .await?;
    return Ok(());
}

/// Starts the row's capsule supervisor in the background on attach, or records that no state root resolves.
fn start_on_attach(ws: &Workspace, workspaces: &Workspaces, state_root: &Option<std::path::PathBuf>) {
    match state_root.clone() {
        None => {
            ws.set_activation_error(Some(format!(
                "could not resolve this machine's state root ({} unset)",
                crate::rows::spawn::state_root::STATE_ROOT_HINT
            )));
        }
        Some(root) => {
            let workspace_id = ws.workspace_id.clone();
            let workspace_id_for_log = workspace_id.clone();
            let agent_kind = ws.agent();
            let agent_name = ws.agent_name();
            let slug = ws.slug.clone();
            let project_root = ws.project_root.clone();
            let workspaces_for_start = workspaces.clone();
            tokio::spawn(async move {
                wait_for_test_activation_barrier().await;
                let result = tokio::task::spawn_blocking(move || {
                    crate::rows::run::activation::ensure_started(
                        &root,
                        &workspace_id,
                        &agent_kind,
                        &agent_name,
                        &slug,
                        &project_root,
                        crate::rows::run::activation::ActivationIntent::Selection,
                        workspaces_for_start,
                    )
                })
                .await
                .unwrap_or_else(|e| {
                    Err(format!("capsule start-on-attach task panicked: {e}"))
                });
                match result {
                    Ok(Some(())) => {
                        tracing::info!(workspace_id = %workspace_id_for_log, "pty.open: capsule supervisor started on attach");
                    }
                    Ok(None) => {}
                    Err(detail) => {
                        tracing::warn!(workspace_id = %workspace_id_for_log, error = %detail, "pty.open: capsule supervisor start-on-attach failed");
                    }
                }
                record_test_activation_marker("completions");
            });
        }
    }
}

/// Writes one marker file per arrival/completion/wait-for-settle-cycle
/// under `<barrier path>.<kind>/`, so a test can poll an exact count
/// instead of inferring one from timing. No-op unless `SOT_TEST_ACTIVATION_
/// BARRIER` is set.
pub(crate) fn record_test_activation_marker(kind: &str) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    let dir = std::path::PathBuf::from(format!("{barrier_path}.{kind}"));
    let _ = std::fs::create_dir_all(&dir);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("{}-{seq}", std::process::id())), b"");
}

/// Test-only barrier at the top of `pty.open`'s activation task: when
/// `SOT_TEST_ACTIVATION_BARRIER` names a path, blocks until the test
/// creates that file (not a guessed sleep), giving up past a 30s bound.
/// No-op in production.
async fn wait_for_test_activation_barrier() {
    let Ok(path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    record_test_activation_marker("arrivals");
    let path = std::path::PathBuf::from(path);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !path.is_file() {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(path = ?path, "capsule activation test barrier: released by timeout, not by the test");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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
