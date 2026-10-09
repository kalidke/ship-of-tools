//! `repl.execute`: run code or a `.jl` file in a row's REPL and answer with the whole report.

use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use sot_protocol::ReplErrorOut;
use sot_protocol::ReplExecuteInput;
use sot_protocol::ReplExecuteReq;
use sot_protocol::ReplExecuteRes;
use sot_protocol::ReplValueOut;
use sot_protocol::StackFrame;
use tokio::sync::broadcast;
use crate::sidecars::repl::ReplFrameMsg;
use crate::session::Session;
use crate::rows::Workspaces;
use crate::server::reply::HandlerOutput;

/// Backend-issued eval_id space for `repl.execute` runs (ADR 0033). Starts at
/// 2^40 so it never collides with a frontend's small per-workspace
/// `repl.eval` counter, while staying a positive integer well under 2^53 (safe
/// for JSON/`jq` consumers) — unlike a high-bit-set id. The `run_id` string
/// returned to the caller is derived from it.
static EXEC_EVAL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

fn next_exec_eval_id() -> u64 {
    EXEC_EVAL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const EXEC_DEFAULT_TIMEOUT_MS: u64 = 120_000;
const EXEC_MIN_TIMEOUT_MS: u64 = 1_000;
const EXEC_MAX_TIMEOUT_MS: u64 = 1_800_000;
/// Per-field inline cap for `value` / `error` text — stdout/stderr are already
/// bounded by `EXEC_TEXT_CAP` in the collector; this guards against one giant
/// `show` repr blowing the 1 MiB envelope.
const EXEC_FIELD_CAP: usize = 64 * 1024;

fn exec_truncate_field(s: &mut String) {
    if s.len() > EXEC_FIELD_CAP {
        let mut cut = EXEC_FIELD_CAP;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n…[truncated]");
    }
}

fn exec_mime_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/svg+xml" => "svg",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn exec_err_frame(req_id: u64, run_id: &str, ws_id: &str, outcome: &str, msg: String) -> HandlerOutput {
    let res = ReplExecuteRes {
        run_id: run_id.to_string(),
        workspace_id: ws_id.to_string(),
        outcome: outcome.to_string(),
        elapsed_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        values: Vec::new(),
        error: Some(ReplErrorOut {
            message: msg,
            stacktrace: Vec::new(),
        }),
        figures: Vec::new(),
        truncated: false,
        project_dir: None,
        project_source: None,
    };
    vec![(
        Frame::res(
            req_id,
            op::REPL_EXECUTE,
            serde_json::to_value(res).unwrap_or_else(|_| json!({})),
        ),
        None,
    )]
}

/// Builds the inner REPL op, its payload and the drawer display, refusing a `run_file` path outside the root.
fn build_exec_request(req_id: u64, req: &ReplExecuteReq, ws: &crate::rows::Workspace, eval_id: u64,
    run_id: &str, ws_id: &str) -> std::result::Result<(&'static str, serde_json::Value, String), HandlerOutput> {
    Ok(match &req.input {
        ReplExecuteInput::RunFile { path } => {
            let joined = {
                let pb = std::path::PathBuf::from(path);
                if pb.is_absolute() {
                    pb
                } else {
                    ws.project_root.join(pb)
                }
            };
            let abs = match joined.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return Err(exec_err_frame(
                        req_id,
                        &run_id,
                        &ws_id,
                        "error",
                        format!("cannot resolve path {path:?}: {e}"),
                    ))
                }
            };
            let root = ws.project_root.canonicalize().unwrap_or_else(|_| ws.project_root.clone());
            if !abs.starts_with(&root) {
                return Err(exec_err_frame(
                    req_id,
                    &run_id,
                    &ws_id,
                    "error",
                    format!(
                        "repl run is confined to the workspace root ({}); {} is outside it — \
                         use `repl eval --code 'include(\"{}\")'` for files elsewhere",
                        root.display(),
                        abs.display(),
                        abs.display(),
                    ),
                ));
            }
            if !abs.is_file() || abs.extension().and_then(|s| s.to_str()) != Some("jl") {
                return Err(exec_err_frame(
                    req_id,
                    &run_id,
                    &ws_id,
                    "error",
                    format!("not an existing .jl file: {}", abs.display()),
                ));
            }
            let disp = format!(
                "run {}",
                abs.file_name().and_then(|s| s.to_str()).unwrap_or("?.jl")
            );
            (
                op::REPL_RUN_FILE,
                json!({
                    "eval_id": eval_id,
                    "path": abs.to_string_lossy(),
                    "fresh": false,
                    "workspace_id": ws_id,
                }),
                disp,
            )
        }
        ReplExecuteInput::Eval { code, mode } => {
            let mut p = json!({ "eval_id": eval_id, "code": code, "workspace_id": ws_id });
            if let Some(m) = mode {
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("mode".to_string(), json!(m));
                }
            }
            let first = code.lines().next().unwrap_or("").trim();
            let disp = if first.chars().count() > 60 {
                format!("{}…", first.chars().take(60).collect::<String>())
            } else {
                first.to_string()
            };
            (op::REPL_EVAL, p, disp)
        }
    })
}

/// Broadcasts the `started` control frame that pre-registers the run in the drawer.
fn announce_exec_started(req: &ReplExecuteReq, ws: &crate::rows::Workspace, workspaces: &Workspaces,
    eval_id: u64, run_id: &String, display: String) -> (String, broadcast::Sender<ReplFrameMsg>) {
    let origin = req.origin.clone().unwrap_or_else(|| "session".to_string());
    let frame_ws = ws.slug.clone();
    let frame_tx = workspaces.repl_frame_tx();
    let _ = frame_tx.send(ReplFrameMsg {
        eval_id,
        workspace_id: Some(frame_ws.clone()),
        frame: json!({
            "kind": "started",
            "run_id": run_id.clone(),
            "origin": origin,
            "display": display,
        }),
    });
    (frame_ws, frame_tx)
}

/// Waits for the shim's res within the request's budget; returns the elapsed time, the base outcome and the res.
async fn await_exec_reply(req: &ReplExecuteReq,
    reply_rx: tokio::sync::oneshot::Receiver<Result<serde_json::Value>>)
    -> (u64, &'static str, Option<serde_json::Value>) {
    let timeout_ms = req
        .timeout_ms
        .unwrap_or(EXEC_DEFAULT_TIMEOUT_MS)
        .clamp(EXEC_MIN_TIMEOUT_MS, EXEC_MAX_TIMEOUT_MS);
    let start = std::time::Instant::now();
    let awaited = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), reply_rx).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Base terminal state from the await. On timeout we deliberately do NOT
    // send an interrupt (that could race and kill a subsequent user eval — the
    // run keeps going and its frames still reach the drawer).
    let (base_outcome, res_payload): (&str, Option<serde_json::Value>) = match awaited {
        Ok(Ok(Ok(v))) => ("completed", Some(v)),
        Ok(Ok(Err(_))) => ("repl_died", None),
        Ok(Err(_)) => ("repl_died", None),
        Err(_) => ("timeout", None),
    };
    (elapsed_ms, base_outcome, res_payload)
}

/// Reads the terminal error code and the project the shim reported out of its res.
fn read_exec_res(res_payload: Option<serde_json::Value>)
    -> (bool, Option<ReplErrorOut>, Option<String>, Option<String>) {
    let mut res_code_error = false;
    let mut error_out: Option<ReplErrorOut> = None;
    let mut project_dir: Option<String> = None;
    let mut project_source: Option<String> = None;
    if let Some(res) = &res_payload {
        project_dir = res.get("project_dir").and_then(|v| v.as_str()).map(String::from);
        project_source = res.get("project_source").and_then(|v| v.as_str()).map(String::from);
        if let Some(code) = res.get("code").and_then(|v| v.as_str()) {
            res_code_error = true;
            let msg = res.get("error").and_then(|v| v.as_str()).unwrap_or(code).to_string();
            error_out = Some(ReplErrorOut { message: msg, stacktrace: Vec::new() });
        }
    }
    (res_code_error, error_out, project_dir, project_source)
}

/// Splits the collected frames into text, values, images and the strongest error kind; the first error frame fills `error_out` if still empty.
fn split_exec_frames(frames: Vec<serde_json::Value>, error_out: &mut Option<ReplErrorOut>)
    -> (String, String, Vec<ReplValueOut>, Vec<(String, String)>, Option<&'static str>) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut values: Vec<ReplValueOut> = Vec::new();
    let mut image_frames: Vec<(String, String)> = Vec::new();
    let mut frame_error_kind: Option<&str> = None;
    for f in &frames {
        match f.get("kind").and_then(|v| v.as_str()) {
            Some("stdout") => {
                if let Some(t) = f.get("text").and_then(|v| v.as_str()) {
                    stdout.push_str(t);
                }
            }
            Some("stderr") => {
                if let Some(t) = f.get("text").and_then(|v| v.as_str()) {
                    stderr.push_str(t);
                }
            }
            Some("value") => {
                let mime = f.get("mime").and_then(|v| v.as_str()).unwrap_or("text/plain").to_string();
                let mut text = f.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                exec_truncate_field(&mut text);
                values.push(ReplValueOut { mime, text });
            }
            Some("image") => {
                let mime = f.get("mime").and_then(|v| v.as_str()).unwrap_or("image/png").to_string();
                if let Some(b64) = f.get("data_base64").and_then(|v| v.as_str()) {
                    image_frames.push((mime, b64.to_string()));
                }
            }
            Some("error") => {
                let message = f.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let k = if message.contains("REPL busy") {
                    "busy"
                } else if message.contains("InterruptException") {
                    "interrupted"
                } else {
                    "error"
                };
                // Strongest-wins: busy > interrupted > error.
                frame_error_kind = Some(match (frame_error_kind, k) {
                    (Some("busy"), _) | (_, "busy") => "busy",
                    (Some("interrupted"), _) | (_, "interrupted") => "interrupted",
                    _ => "error",
                });
                if error_out.is_none() {
                    let stack: Vec<StackFrame> = f
                        .get("stacktrace")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                    let mut msg = message.clone();
                    exec_truncate_field(&mut msg);
                    *error_out = Some(ReplErrorOut { message: msg, stacktrace: stack });
                }
            }
            _ => {}
        }
    }
    (stdout, stderr, values, image_frames, frame_error_kind)
}

/// Writes the image frames under the run's folder and returns their paths.
async fn spill_exec_figures(ws: &crate::rows::Workspace, run_id: &String, image_frames: Vec<(String, String)>)
    -> Vec<String> {
    let mut figures: Vec<String> = Vec::new();
    if !image_frames.is_empty() {
        let runs_dir = ws.project_root.join(".sot").join("runs").join(&run_id);
        let run_id_blk = run_id.clone();
        let spill = tokio::task::spawn_blocking(move || -> std::result::Result<Vec<String>, String> {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine as _;
            std::fs::create_dir_all(&runs_dir).map_err(|e| format!("create {runs_dir:?}: {e}"))?;
            let mut out = Vec::new();
            for (i, (mime, b64)) in image_frames.iter().enumerate() {
                let bytes = STANDARD.decode(b64).map_err(|e| format!("fig {i} base64: {e}"))?;
                let p = runs_dir.join(format!("fig-{i}.{}", exec_mime_ext(mime)));
                std::fs::write(&p, &bytes).map_err(|e| format!("write {p:?}: {e}"))?;
                out.push(p.to_string_lossy().into_owned());
            }
            Ok(out)
        })
        .await;
        match spill {
            Ok(Ok(paths)) => figures = paths,
            Ok(Err(e)) => tracing::warn!(run_id = %run_id_blk, "figure spill failed: {e}"),
            Err(e) => tracing::warn!(run_id = %run_id_blk, "figure spill task panicked: {e}"),
        }
    }
    figures
}

/// Closes the drawer run `announce_exec_started` opened, with a `done` frame the shim did not send.
fn close_exec_run(frame_tx: &tokio::sync::broadcast::Sender<ReplFrameMsg>, frame_ws: &str, eval_id: u64, elapsed_ms: u64) {
    let _ = frame_tx.send(ReplFrameMsg {
        eval_id,
        workspace_id: Some(frame_ws.to_string()),
        frame: json!({ "kind": "done", "eval_id": eval_id, "elapsed_ms": elapsed_ms }),
    });
}

/// `repl.execute` (ADR 0033): run a `.jl` file (or code chunk) in a workspace's
/// persistent REPL and return the COLLECTED output as one authoritative
/// response. See `op::REPL_EXECUTE`. The output is gathered off a dedicated
/// per-run collector in the supervisor (loss-free, unlike the broadcast bus),
/// completion keys off the shim's terminal `res` (reliable even when no `done`
/// frame is emitted), figures spill to `<ws>/.sot/runs/<run_id>/`, and a
/// timeout returns `outcome:"timeout"` WITHOUT interrupting the run.
pub async fn handle_repl_execute(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ReplExecuteReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::REPL_EXECUTE,
                    json!({ "error": format!("bad repl.execute payload: {e}"), "code": "bad_request" }),
                ),
                None,
            )]);
        }
    };

    let eval_id = next_exec_eval_id();
    let run_id = format!("exec-{eval_id}");
    let ws_id = req.workspace_id.clone();
    tracing::info!(workspace_id = %ws_id, run_id = %run_id, "repl.execute");

    let Some(ws) = workspaces.resolve(Some(ws_id.as_str())) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::REPL_EXECUTE,
                json!({ "error": format!("unknown workspace: {ws_id}"), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };

    // Build the inner op + payload + drawer display; validate a run_file path.
    let (inner_op, inner_payload, display) = match build_exec_request(req_id, &req, &ws, eval_id, &run_id, &ws_id) {
        Ok(request) => request,
        Err(out) => return Ok(out),
    };

    // Phase 2 (ADR 0033): broadcast a `started` control frame so an attached
    // front-end pre-registers this run in the user's drawer (submission order),
    // then routes the streamed output frames + terminal `done` to that entry.
    // Stamp the workspace SLUG, not the canonical `workspace_id`: the FE keys its
    // active workspace + repl snapshots by slug (`current_workspace_key()`), and
    // the `started` handler is the one place that compares the frame's ws against
    // that key to pick which drawer the entry pre-registers in. Output frames
    // route by `eval_id` (their ws hint is ignored), so they still land on the
    // same entry. Stamping the canonical id here made that compare never match →
    // the entry was dropped down the "no snapshot" path and every session run
    // orphaned as "repl.frame dropped: no in-flight entry".
    let (frame_ws, frame_tx) = announce_exec_started(&req, &ws, workspaces, eval_id, &run_id, display);

    let repl = ws.repl(workspaces.repl_frame_tx());
    let submitted_at = std::time::Instant::now();
    let (reply_rx, collector) = match repl.execute(inner_op, inner_payload).await {
        Ok(x) => x,
        Err(e) => {
            close_exec_run(&frame_tx, &frame_ws, eval_id, submitted_at.elapsed().as_millis() as u64);
            return Ok(exec_err_frame(
                req_id,
                &run_id,
                &ws_id,
                "repl_died",
                format!("repl submit failed: {e:#}"),
            ))
        }
    };

    let (elapsed_ms, base_outcome, res_payload) = await_exec_reply(&req, reply_rx).await;

    // Snapshot the loss-free collector.
    let (frames, truncated) = {
        let acc = collector.lock().unwrap_or_else(|e| e.into_inner());
        (acc.frames.clone(), acc.truncated)
    };
    let shim_closed_run = frames.iter().any(|f| f.get("kind").and_then(|v| v.as_str()) == Some("done"));

    // Terminal error carried by the shim's res (bad_request / io_error /
    // repl_exception) — authoritative over frame inspection.
    let (res_code_error, mut error_out, project_dir, project_source) = read_exec_res(res_payload);

    // Split collected frames.
    let (stdout, stderr, values, image_frames, frame_error_kind) = split_exec_frames(frames, &mut error_out);

    // Final outcome precedence: timeout / repl_died (from the await) win, then a
    // shim res error code, then frame classification (busy > interrupted >
    // error), else ok.
    let outcome: &str = match base_outcome {
        "timeout" => "timeout",
        "repl_died" => "repl_died",
        _ if res_code_error => "error",
        _ => frame_error_kind.unwrap_or("ok"),
    };

    // Phase 2: every report closes the run it announced. The shim's own `done`
    // is the close when the collector holds one; otherwise (a timeout, a dead
    // child, or a terminal res with no frames) the handler supplies it.
    if !shim_closed_run {
        close_exec_run(&frame_tx, &frame_ws, eval_id, elapsed_ms);
    }

    // Spill figures to files so the response never inlines base64 (1 MiB cap).
    let figures = spill_exec_figures(&ws, &run_id, image_frames).await;

    let res = ReplExecuteRes {
        run_id: run_id.clone(),
        workspace_id: ws_id.clone(),
        outcome: outcome.to_string(),
        elapsed_ms,
        stdout,
        stderr,
        values,
        error: error_out,
        figures,
        truncated,
        project_dir,
        project_source,
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::REPL_EXECUTE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
