#![cfg(target_os = "linux")]
//! A window's selection, end to end: a real `sotd` and a real `sot-capsule` start a Claude row whose `claude` is a stub
//! that records its environment and sleeps, a window connection sends `selection.set`, and the row's prompt hook
//! (`agents/sot-selection-hook.sh`, run with the stub's recorded environment) prints the selection; a clear, and the
//! window's connection ending, each leave the hook printing nothing. No agent program runs and no live daemon is touched.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use std::path::Path;
use std::time::Duration;

use sot_protocol::ops::{Drawing, InkBase, SelectedText, SelectionSetReq, TextPos};
use sot_protocol::{op, selection_digest, HelloReq};
use support::{call, connect_and_hello, poll_for_phase, poll_until, sot_capsule_exe, try_connect, Conn, Env, BOUND};

/// A `claude` that records its environment, whole, in `record` (published by a rename) and sleeps.
fn write_recording_claude(dir: &Path, record: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let script = format!("#!/bin/sh\nenv > '{r}.part' && mv '{r}.part' '{r}'\nexec sleep 120\n", r = record.display());
    sot_platform::test_exec::write_executable(&dir.join("claude"), script);
}

/// A hello as a window's: the role `fe` and a name, which is what `selection.set` requires of its connection.
async fn connect_as_window(env: &Env) -> (Conn, u64) {
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "sotd's socket").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let mut hello = HelloReq::this_process("selection-test", "fe", Some("host-a".to_string())).expect("this process's account");
    hello.name = Some("selection-window".to_string());
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    (conn, 2)
}

/// `selection.set` for `workspace`: the selection of `text` over line `line` of `a.txt`, or a clear.
async fn set(conn: &mut Conn, id: u64, workspace: &str, text: Option<(u32, &str)>) -> serde_json::Value {
    let selection = text.map(|(line, shown)| SelectedText {
        node_id: "files:a.txt".to_string(),
        start: TextPos { line, byte: 0 },
        end: TextPos { line, byte: shown.len() as u32 },
        len: shown.len() as u32,
        digest: selection_digest(shown),
    });
    let req = SelectionSetReq { workspace_id: workspace.to_string(), selection, drawing: None };
    call(conn, id, op::SELECTION_SET, serde_json::to_value(req).unwrap()).await.payload
}

/// What the row's prompt hook prints when the leg's recorded environment runs it.
fn run_hook(record: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents/sot-selection-hook.sh");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(script).env_clear().stdin(std::process::Stdio::null());
    for line in record.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                cmd.env(k, v);
            }
        }
    }
    sot_platform::test_wait::run_within(&mut cmd, BOUND).ok().stdout
}

/// A started Claude row in `env`'s project (holding `a.txt` and `fig.png`) whose stub `claude` recorded its environment:
/// the daemon's connection, the row id and the recorded environment.
async fn start_row(env: &Env) -> (Conn, String, String) {
    assert!(sot_capsule_exe().is_file(), "sot-capsule not found next to sotd: build it first");
    // the Claude layer, so the leg's recipe carries the hooks and the row's marker is written
    let skill = env.comm_root.join("claude-layer/.claude/skills/sot-session-start");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: sot-session-start\n---\n").unwrap();
    let record = env._tmp.path().join("leg.env");
    let stub = env._tmp.path().join("stubbin");
    write_recording_claude(&stub, &record);
    std::fs::write(env.workspace_project_root.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    image::RgbImage::from_pixel(64, 48, image::Rgb([10, 200, 30])).save(env.workspace_project_root.join("fig.png")).unwrap();
    env.spawn_sotd_with_path_and_env(&stub, &[]);

    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "selhook-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    // ready: the launch is settled, so the marker has been written
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    let recorded = poll_until(|| async { std::fs::read_to_string(&record).ok() }, BOUND, "the stub claude's recorded environment").await;
    assert!(recorded.lines().any(|l| l.starts_with("SOT_SELECTION_FILE=")), "the leg names its selection file: {recorded}");
    assert_eq!(run_hook(&recorded), "", "before any selection the hook prints nothing");

    (conn, ws, recorded)
}

#[tokio::test]
async fn a_window_selection_reaches_the_rows_prompt_hook_and_its_end_withdraws_it() {
    let env = Env::new("selhook");
    let (_control, ws, recorded) = start_row(&env).await;

    // 1. a window's selection is shared, and the hook prints it
    let (mut window, mut window_id) = connect_as_window(&env).await;
    assert_eq!(set(&mut window, window_id, &ws, Some((1, "beta"))).await["state"], "shared", "the leg has the hook");
    window_id += 1;
    let printed: serde_json::Value = serde_json::from_str(&run_hook(&recorded)).expect("the hook prints one JSON object");
    assert_eq!(printed["systemMessage"], "Selection: a.txt lines 2-2");
    let context = printed["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains("lines 2 to 2") && context.contains("\nbeta\n"), "the model is given the text: {context}");

    // 2. a clear: the hook prints nothing
    assert_eq!(set(&mut window, window_id, &ws, None).await["state"], "shared");
    window_id += 1;
    assert_eq!(run_hook(&recorded), "", "a clear withdraws the selection");

    // 3. a second selection, then the window's connection ends: the hook prints nothing again
    assert_eq!(set(&mut window, window_id, &ws, Some((2, "gamma"))).await["state"], "shared");
    assert!(run_hook(&recorded).contains("gamma"), "the second selection");
    drop(window);
    poll_until(|| async { run_hook(&recorded).is_empty().then_some(()) }, BOUND, "the window's end to withdraw its selection").await;
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn a_window_drawing_reaches_the_rows_prompt_hook_and_its_end_withdraws_it() {
    let env = Env::new("seldraw");
    let (_control, ws, recorded) = start_row(&env).await;
    let (mut window, window_id) = connect_as_window(&env).await;

    // a drawing over the figure is shared, and the hook names a picture that decodes
    let drawing = Drawing { base: InkBase::Figure { node_id: "files:fig.png".to_string() }, strokes: vec![vec![[20_000, 20_000], [40_000, 40_000]]] };
    let req = SelectionSetReq { workspace_id: ws.clone(), selection: None, drawing: Some(drawing) };
    let reply = call(&mut window, window_id, op::SELECTION_SET, serde_json::to_value(req).unwrap()).await.payload;
    assert_eq!(reply["state"], "shared", "{reply}");
    let printed: serde_json::Value = serde_json::from_str(&run_hook(&recorded)).expect("the hook prints one JSON object");
    assert_eq!(printed["systemMessage"], "Drawing: fig.png (1 stroke)");
    let context = printed["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    let picture = context.split(" is ").nth(1).and_then(|t| t.split(" (drawing ").next()).expect("the picture's path");
    assert!(picture.ends_with(&format!(".sot/captures/ink-{ws}.png")), "{picture}");
    assert_eq!(image::image_dimensions(picture).expect("the picture decodes"), (64, 48));

    // the window's connection ending withdraws it; the picture stays
    drop(window);
    poll_until(|| async { run_hook(&recorded).is_empty().then_some(()) }, BOUND, "the window's end to withdraw its drawing").await;
    assert!(Path::new(picture).is_file(), "the picture stays");
    env.kill_daemon_bounded().await;
}
