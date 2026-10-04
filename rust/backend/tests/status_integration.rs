#![cfg(any(windows, target_os = "linux"))]
//! `sotd status` (topology plan §E) against a REAL daemon this box starts
//! and stops — the live half's own proof, complementing `status_cli`'s
//! fixture-only unit tests (`report`/`render_text`/`render_json`, which
//! never touch a daemon at all). `mod support;` reuses `Env` for the tmp
//! dirs and bounded teardown; the daemon itself is spawned BY HAND here
//! (not `Env::spawn_sotd`) because it must listen on the socket the box's
//! OWN daemon label derives — the exact endpoint `sotd status`'s own
//! `topology::endpoint::local_endpoint()` dials, and the reason both spell that label
//! `local_daemon_label()` rather than a literal — rather than
//! `spawn_sotd`'s arbitrary per-test `--socket` path.

mod support;

use std::process::Stdio;
use std::time::Duration;

use sot_protocol::{op, HelloReq};
use support::{call, poll_until, sotd_exe, try_connect, Env, TEST_STATE_HOST};

const BOUND: Duration = Duration::from_secs(20);

/// Real-process tests share one CI runner; serialize them like every other
/// file in this crate that spawns a real `sotd` (`capsule_workspaces.rs`'s
/// own `SERIAL`). One test today, but the convention is free insurance
/// against a future second test racing `Env::new`'s process-wide
/// `SOT_RUNTIME_DIR` env var.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn sotd_status_reaches_a_real_daemon_and_lists_its_own_row_and_client() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("status");
    // The label-derived socket lives under `$XDG_RUNTIME_DIR`
    // (`runtime_sot_dir`), not `SOT_RUNTIME_DIR`. Left inherited, this test
    // bound the developer's own `<runtime>/sot/sessions/sot.sock`, and the
    // bind unlinked the live daemon's socket. This process and both
    // children use this env's private runtime dir instead.
    std::env::set_var("XDG_RUNTIME_DIR", env._runtime_tmp.path());
    // This process derives the identical path the daemon below binds from
    // the same label, the one `sotd status`'s own `local_endpoint()` dials;
    // checked BEFORE the spawn, so a regression never reaches a real socket.
    let socket_path = sot_protocol::session_socket_path(sot_protocol::local_daemon_label());
    #[cfg(unix)]
    assert!(
        socket_path.starts_with(env._runtime_tmp.path()),
        "the test daemon's socket must sit in this env's private runtime dir, never the developer's: {socket_path:?}"
    );

    // A one-host topology: this box is both the hub and its only daemon.
    let hosts_toml = env._tmp.path().join("hosts.toml");
    std::fs::write(&hosts_toml, format!("hub = \"{TEST_STATE_HOST}\"\n\n[host.{TEST_STATE_HOST}]\ndaemon = true\n")).expect("write hosts.toml");

    let child = std::process::Command::new(sotd_exe())
        .arg("--label")
        .arg(sot_protocol::local_daemon_label())
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
        // `local_endpoint()`'s new precedence (isolation-plan.md §3 C10's
        // Rust half) reads these two before falling back to `--label`; a
        // suite run from inside a Ship of Tools session inherits both, and
        // without this they would point this "spawned daemon" at the live
        // daemon's own socket instead of the temporary endpoint below —
        // the same reason `stdio_bridge.rs`'s daemon spawn strips them.
        .env_remove("SOT_SOCKET")
        .env_remove("SOT_BACKEND_LABEL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sotd");
    env.daemon.borrow_mut().replace(child);

    let stream = poll_until(|| async { try_connect(&socket_path).await }, BOUND, "sotd's own-label socket to accept a connection").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        client_id: "status-it-fe".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some(TEST_STATE_HOST.to_string()),
        role: "fe".to_string(),
        instance: None,
        name: Some(format!("fe@{TEST_STATE_HOST}")),
    };
    // `call` writes the request then skips any evt broadcast (a
    // comm-registry poll included) that legitimately arrives before the
    // matching `res` — a bare next-frame read here would treat such a
    // broadcast as the reply, either failing the "no error" assert
    // wrongly or, worse, passing it vacuously.
    let frame = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);

    // A real turn of input, so this connection is also the daemon's
    // resolved active frontend (`clients.rs::resolve_active`) — proves the
    // ACTIVE marker in `sotd status`'s output, not just bare presence.
    let frame = call(&mut conn, 2, op::FE_PRESENCE, serde_json::json!({})).await;
    assert!(frame.payload.get("error").is_none(), "fe.presence refused: {:?}", frame.payload);

    // `sotd status --json` — a SEPARATE, real one-shot process (the same
    // binary, the same env), not a library call into `status_cli` — this
    // is the actual CLI a launcher or the `sot-status` skill would run.
    let mut status_cmd = tokio::process::Command::new(sotd_exe());
    status_cmd
        .arg("status")
        .arg("--json")
        .env("LOCALAPPDATA", &env.state_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("SOT_SELF_HOST", TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .env("SOT_HOSTS", &hosts_toml)
        // `sotd status`'s own `local_endpoint()` call — see the daemon
        // spawn's comment above; this is the OTHER of the two spawns that
        // must not inherit either variable.
        .env_remove("SOT_SOCKET")
        .env_remove("SOT_BACKEND_LABEL")
        .stdin(Stdio::null());
    let out = tokio::time::timeout(BOUND, status_cmd.output()).await.expect("sotd status did not exit within BOUND").expect("spawn sotd status");
    assert!(out.status.success(), "sotd status exited {:?}: stderr {}", out.status, String::from_utf8_lossy(&out.stderr));

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("sotd status --json did not print valid json: {e}\n{}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(v["hub_reachable"], true, "{v:#}");
    let row = v["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["host"] == TEST_STATE_HOST))
        .unwrap_or_else(|| panic!("no row for {TEST_STATE_HOST}: {v:#}"));
    assert_eq!(row["daemon_state"], "up", "{row:#}");
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
