#![cfg(target_os = "linux")]
//! One meaning of `filed` (ADR 0049, Sending): the staged `comm-send.sh`, run against a
//! real `sotd`, prints `filed` only for a handle whose `last_seen` is under ten minutes old,
//! and the daemon that runs a row keeps that stamp fresh, so an idle row stays live.
//!
//! Run `cargo build -p sot-log --bin sot-capsule` into the same target dir first, as for
//! `capsule_workspaces`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use sot_protocol::op;

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;
use support::*;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A running `sotd` with the staged scripts, and the environment a session of this box runs them in.
struct Staged {
    env: Env,
    bin: PathBuf,
}

impl Staged {
    /// Waits for the hub's lock record and socket, stages the scripts and joins `m4-sender`.
    fn ready(env: Env) -> Self {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !(env.comm_root.join("inbox-lock-manager").exists() && env.socket_path.exists()) {
            if let Some(status) = env.daemon.borrow_mut().as_mut().and_then(|d| d.try_wait().expect("try_wait")) {
                panic!("sotd exited before it was ready: {status}");
            }
            assert!(Instant::now() < deadline, "the hub never recorded its inbox lock and bound its socket");
            std::thread::sleep(Duration::from_millis(100));
        }
        let bin = env._tmp.path().join("bin");
        let staged = Command::new("bash")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../comm/tests/stage-bin.sh"))
            .arg(&bin)
            .output()
            .expect("stage-bin");
        assert!(staged.status.success(), "staging failed: {}", String::from_utf8_lossy(&staged.stdout));
        let this = Self { env, bin };
        let join = this.script("comm-join.sh", &["--name", "m4-sender"]);
        assert!(join.status.success(), "join: {} {}", String::from_utf8_lossy(&join.stdout), String::from_utf8_lossy(&join.stderr));
        this
    }

    /// One staged script, run as a session of this box would run it.
    fn script(&self, name: &str, args: &[&str]) -> Output {
        let cwd = self.env._tmp.path().join("m4");
        std::fs::create_dir_all(&cwd).expect("mkdir cwd");
        Command::new("timeout")
            .arg("60")
            .arg("bash")
            .arg(self.bin.join(name))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.env.home_root)
            .env("SOT_COMM_HOME", &self.env.comm_root)
            .env("SOT_COMM_SELF_FILE", self.env._tmp.path().join("self.txt"))
            .env("SOT_COMM_TEST_HOST", TEST_STATE_HOST)
            .env("SOT_SOCKET", &self.env.socket_path)
            .env("XDG_RUNTIME_DIR", self.env._runtime_tmp.path())
            .env("TMPDIR", self.env._tmp.path())
            .current_dir(cwd)
            .output()
            .expect("run script")
    }

    fn registry(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.env.comm_root.join("registry.json")).expect("registry")).expect("registry json")
    }

    /// Adds registry entries (host `testhost`, the given `last_seen`) under the registry lock, as the daemon's own
    /// registry writes take it.
    fn add_entries(&self, entries: &[(&str, String)]) {
        support::write_registry(&self.env.comm_root, |reg| {
            for (handle, last_seen) in entries {
                reg["agents"][*handle] = serde_json::json!({"host": TEST_STATE_HOST, "last_seen": last_seen});
            }
        });
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for `now - ago` seconds, from the box's own `date`.
fn stamp(ago: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let out = Command::new("date")
        .args(["-u", "-d", &format!("@{}", now - ago), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("date");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a_send_to_a_handle_no_live_session_holds_is_failed_and_appends_nothing() {
    let _serial = SERIAL.blocking_lock();
    let env = Env::new("cs1");
    env.spawn_sotd();
    let b = Staged::ready(env);
    b.add_entries(&[("m4-gone", stamp(3600)), ("m4-live", stamp(0))]);

    let gone = b.script("comm-send.sh", &["@m4-gone", "into the void"]);
    let (out, err) = (String::from_utf8_lossy(&gone.stdout), String::from_utf8_lossy(&gone.stderr));
    assert_eq!(gone.status.code(), Some(1), "stdout {out} stderr {err}");
    assert!(err.contains("FAILED -> @m4-gone: no live session holds @m4-gone"), "stdout {out} stderr {err}");
    assert!(!out.contains("filed"), "stdout {out}");
    assert!(!b.env.comm_root.join("inbox/m4-gone.jsonl").exists(), "a line was appended for a handle nobody holds");

    let live = b.script("comm-send.sh", &["@m4-live", "hello"]);
    let (out, err) = (String::from_utf8_lossy(&live.stdout), String::from_utf8_lossy(&live.stderr));
    assert_eq!(live.status.code(), Some(0), "stdout {out} stderr {err}");
    assert!(out.contains("filed -> @m4-live"), "stdout {out} stderr {err}");
    let inbox = std::fs::read_to_string(b.env.comm_root.join("inbox/m4-live.jsonl")).expect("inbox");
    assert_eq!(inbox.lines().count(), 1, "{inbox}");
    let line: serde_json::Value = serde_json::from_str(inbox.lines().next().unwrap()).unwrap();
    assert_eq!(line["from"], "m4-sender");
    assert_eq!(line["msg"], "hello");
}

/// A stub `claude` that stays up: the row it backs is Ready for as long as the daemon runs it.
fn write_sleeping_claude(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    sot_log::test_exec::write_executable(&dir.join("claude"), "#!/bin/sh\nexec sleep 600\n");
}

/// An idle row's handle has a stale entry. Its daemon stamps `last_seen` while the row runs, so a send
/// is filed, even with that daemon down: liveness is the registry's one fact, not a question for a daemon.
#[tokio::test]
async fn an_idle_row_stays_live_while_its_daemon_is_down() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");
    let env = Env::new("cs2");
    let stub_dir = env._tmp.path().join("stubbin");
    write_sleeping_claude(&stub_dir);
    env.spawn_sotd_with_path_and_env(&stub_dir, &[]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let b = Staged::ready(env);
    // The entry exists before the row does: the daemon only ever stamps an entry that is there.
    b.add_entries(&[("m4-idle", stamp(3600))]);

    let create = serde_json::json!({
        "label": "m4-idle-row",
        "project_root": b.env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let join = call(&mut conn, next_id, op::AGENT_JOIN, serde_json::json!({ "workspace_id": ws, "handle": "m4-idle" })).await;
    next_id += 1;
    assert_eq!(join.payload["ok"], true, "agent.join failed: {:?}", join.payload);
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", Duration::from_secs(60)).await;

    let cutoff = stamp(120);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let seen = b.registry()["agents"]["m4-idle"]["last_seen"].as_str().unwrap_or_default().to_string();
        if seen > cutoff {
            break;
        }
        assert!(Instant::now() < deadline, "the daemon never stamped the running row's handle (last_seen {seen})");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    b.env.kill_daemon_bounded().await;

    let sent = b.script("comm-send.sh", &["@m4-idle", "while its daemon is down"]);
    let (out, err) = (String::from_utf8_lossy(&sent.stdout), String::from_utf8_lossy(&sent.stderr));
    assert_eq!(sent.status.code(), Some(0), "stdout {out} stderr {err}");
    assert!(out.contains("filed -> @m4-idle"), "stdout {out} stderr {err}");
    let inbox = std::fs::read_to_string(b.env.comm_root.join("inbox/m4-idle.jsonl")).expect("inbox");
    assert_eq!(inbox.lines().count(), 1, "{inbox}");
    let line: serde_json::Value = serde_json::from_str(inbox.lines().next().unwrap()).unwrap();
    assert_eq!(line["msg"], "while its daemon is down");
}
