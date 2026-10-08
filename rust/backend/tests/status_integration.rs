#![cfg(any(windows, target_os = "linux"))]
//! `sotd status` (topology plan §E) against a REAL daemon this box starts
//! and stops — the live half's own proof, complementing `topology::status`'s
//! fixture-only unit tests (`report`/`render_text`/`render_json`, which
//! never touch a daemon at all). `mod support;` reuses `Env` for the tmp
//! dirs and bounded teardown. `sotd status` finds a daemon the way it finds
//! this box's own, through `topology::endpoint::local_endpoint()`, which
//! derives the endpoint from a label; so the daemon here starts at a label
//! of this test's own (`support::own_label`), and every `sotd` client reads
//! that label from `SOT_BACKEND_LABEL`. On Windows a label's endpoint is the
//! per-user pipe `\\.\pipe\sot-<user>-<label>`, which no runtime folder
//! moves: the label alone keeps this test off this box's own daemon, and
//! `support::label_endpoint` refuses the default label's before anything
//! starts.

mod support;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sot_protocol::{op, HelloReq};
use support::{call, poll_until, try_connect, Env, TEST_STATE_HOST};

const BOUND: Duration = Duration::from_secs(20);

/// Real-process tests share one CI runner; serialize them like every other
/// file in this crate that spawns a real `sotd` (`capsule_workspaces/main.rs`'s
/// own `SERIAL`). One test today, but the convention is free insurance
/// against a future second test racing `Env::new`'s process-wide
/// `SOT_RUNTIME_DIR` env var.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `sotd <args>` as a client of the daemon at `label`, in `sotd status`'s environment.
fn client(env: &Env, hosts_toml: &Path, label: &str, args: &[&str]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::from(support::sotd_client_of(label));
    cmd.args(args)
        .env("LOCALAPPDATA", &env.state_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("SOT_SELF_HOST", TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .env("SOT_HOSTS", hosts_toml)
        .stdin(Stdio::null());
    cmd
}

#[tokio::test]
async fn sotd_status_reaches_a_real_daemon_and_lists_its_own_row_and_client() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("status");
    // A label's socket lives under `$XDG_RUNTIME_DIR` (`runtime_sot_dir`), not
    // `SOT_RUNTIME_DIR`. This process and every child use this env's private
    // runtime dir, so no socket of this test's is left in the developer's.
    std::env::set_var("XDG_RUNTIME_DIR", env._runtime_tmp.path());
    // This test's own label and the endpoint it derives here: the daemon below
    // binds it and every dial below reaches it.
    let label = support::own_label("status");
    let socket_path = support::label_endpoint(&label);

    // A one-host topology: this box is both the hub and its only daemon.
    let hosts_toml = env._tmp.path().join("hosts.toml");
    std::fs::write(&hosts_toml, format!("hub = \"{TEST_STATE_HOST}\"\n\n[host.{TEST_STATE_HOST}]\ndaemon = true\n")).expect("write hosts.toml");

    // Before anything dials: in `sotd status`'s own environment,
    // `local_endpoint()` derives this test's endpoint. `sotd topology
    // relay-endpoint` prints it (this box is the hub) and dials nothing.
    let out = tokio::time::timeout(
        BOUND,
        client(&env, &hosts_toml, &label, &["topology", "relay-endpoint"]).output(),
    )
    .await
    .expect("sotd topology relay-endpoint did not exit within BOUND")
    .expect("spawn sotd topology relay-endpoint");
    let scheme = if cfg!(windows) { "pipe" } else { "unix" };
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("{scheme}:{}", socket_path.display()),
        "a client of this test's daemon derives another endpoint; stderr {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let child = support::sotd_daemon_at(&label)
        .arg("--project-root")
        .arg(&env.daemon_project_root)
        .env("LOCALAPPDATA", &env.state_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("SOT_SELF_HOST", TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .env("HOME", &env.home_root)
        .env("USERPROFILE", &env.home_root)
        .env("SOT_COMM_HOME", &env.comm_root)
        .env("SOT_HOSTS", &hosts_toml)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sotd");
    eprintln!(
        "status_integration: own daemon pid {} at {}",
        child.id(),
        socket_path.display()
    );
    env.daemon.borrow_mut().replace(child);

    let stream = poll_until(|| async { try_connect(&socket_path).await }, BOUND, "this test's daemon to accept a connection").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        name: Some(format!("fe@{TEST_STATE_HOST}")),
        ..HelloReq::this_process(label.as_str(), "fe", Some(TEST_STATE_HOST.to_string())).expect("this process's account")
    };
    // `call` writes the request then skips any evt broadcast (a
    // comm-registry poll included) that legitimately arrives before the
    // matching `res` — a bare next-frame read here would treat such a
    // broadcast as the reply, either failing the "no error" assert
    // wrongly or, worse, passing it vacuously.
    let frame = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
    assert_eq!(
        frame.payload["label"],
        label.as_str(),
        "the hello reached a daemon at another label: {:?}",
        frame.payload
    );

    // A real turn of input, so this connection is also the daemon's
    // resolved active frontend (`clients.rs::resolve_active`) — proves the
    // ACTIVE marker in `sotd status`'s output, not just bare presence.
    let frame = call(&mut conn, 2, op::FE_PRESENCE, serde_json::json!({})).await;
    assert!(frame.payload.get("error").is_none(), "fe.presence refused: {:?}", frame.payload);

    // `sotd status --json` — a SEPARATE, real one-shot process (the same
    // binary, the same env), not a library call into `topology::status` — this
    // is the actual CLI a launcher or the `sot-status` skill would run.
    let out = tokio::time::timeout(
        BOUND,
        client(&env, &hosts_toml, &label, &["status", "--json"]).output(),
    )
    .await
    .expect("sotd status did not exit within BOUND")
    .expect("spawn sotd status");
    assert!(out.status.success(), "sotd status exited {:?}: stderr {}", out.status, String::from_utf8_lossy(&out.stderr));

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("sotd status --json did not print valid json: {e}\n{}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(v["hub_reachable"], true, "{v:#}");
    let row = v["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["host"] == TEST_STATE_HOST))
        .unwrap_or_else(|| panic!("no row for {TEST_STATE_HOST}: {v:#}"));
    assert_eq!(row["daemon_state"], "up", "{row:#}");
    assert_eq!(
        row["daemon_host"], TEST_STATE_HOST,
        "sotd status reached a daemon that is not this test's: {row:#}"
    );
    assert!(row["rows_total"].as_u64().unwrap_or(0) >= 1, "the default row should be counted: {row:#}");
    let clients = row["clients"].as_array().expect("clients array");
    assert!(
        clients.iter().any(|c| c["role"] == "fe" && c["host"] == TEST_STATE_HOST),
        "the connected fe client should appear: {row:#}"
    );
    assert_eq!(v["active_frontend_of_hub"], format!("fe@{TEST_STATE_HOST}"), "{v:#}");

    drop(conn);
    env.kill_daemon_bounded().await;
}
