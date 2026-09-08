#![cfg(any(windows, target_os = "linux"))]
//! Switch-latency Phase 1: proves a slow request no longer head-of-line-
//! blocks a later cheap request's reply on the SAME connection — through
//! the real wire protocol against a real `sotd`, mirroring
//! `capsule_workspaces.rs`'s own real-process posture (no protocol
//! doubles, no mocked `handle_connection`). Gated off macOS like that file:
//! the daemon's default-row boot path shells out to a real `tmux` server on
//! Linux unless `SOT_TMUX_SOCK` isolates it, and macOS CI runners don't ship
//! `tmux` by default (the project's own "macOS leg runs every Unix test"
//! lesson — `capsule_workspaces.rs`'s header carries the same gate for the
//! same reason).
//!
//! `concept.read` is the op made deterministically slow here, via the
//! test-only `SOT_TEST_SLOW_CONCEPT_READ_MS` knob (`server.rs`,
//! `test_slow_concept_read_delay`): no existing op is slow on demand without
//! something this sandbox can't assume (a real Julia kernel for
//! `preview.get`'s plugin path, a large file already on disk for its
//! bytes-level fallback). `hello` is the cheap op: fully inline, no
//! filesystem or workspace lookup, sent a second time on the same
//! connection as the "B" request.
//!
//! The assertion is ORDER only — which reply is *observed* first — never a
//! wall-clock bound: an absolute ceiling on the cheap reply is flaky on a
//! loaded CI runner, and a floor on the slow reply is unsound (the
//! artificial delay can start running before this test even finishes
//! writing the second request). Order is exactly what the fix guarantees
//! and all this test needs to prove.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// Generous bound for the whole exchange (connect + hello + both requests +
/// the artificial delay), with headroom for a loaded CI runner. Not a
/// precision timing assertion — just the "don't hang forever" backstop every
/// bounded test needs.
const BOUND: Duration = Duration::from_secs(20);

/// The artificial `concept.read` delay this test asks the daemon to inject.
/// Its only job is to keep request A outstanding long enough that request
/// B's reply — sent and read while A is still pending — could not possibly
/// be a same-instant coincidence.
const SLOW_MS: u64 = 500;

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// One isolated `sotd`, rooted at a fresh temp project, with every path it
/// could touch OUTSIDE that tempdir (workspace-registry config, per-machine
/// state, its default row's tmux server) redirected there too — this test
/// must never read or write the developer's real `~/.config/sot`, real
/// state dir, or real tmux server. Isolation recipe copied verbatim from
/// `capsule_workspaces.rs`'s own `Env::spawn_sotd`, trimmed to what this
/// test actually needs: no capsule state-root qualification, no
/// `sot-capsule` dependency, no `SERIAL` lock — every env var below is set
/// only on the CHILD via `.env(...)`, never on this test process itself, so
/// nothing here needs to serialize against another test in this binary.
/// stdout/stderr are left INHERITED (not redirected to null) so a daemon
/// startup failure shows up in this test's own output instead of vanishing.
struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    socket_path: PathBuf,
    tmux_sock: PathBuf,
    daemon: Child,
}

impl Env {
    /// Spawn a real `sotd` with `SOT_TEST_SLOW_CONCEPT_READ_MS` set to
    /// `SLOW_MS` — the ONLY thing distinguishing this daemon from a
    /// production one.
    fn spawn(tag: &str) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("sot-switchlat-")
            .tempdir()
            .expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");

        // Short-prefixed, directly under `/tmp` on Unix (never under `_tmp`,
        // whose own prefix isn't length-bounded) — `sun_path` is 108 bytes
        // including the NUL on Linux, and this test's own wire socket lives
        // directly under this dir. Windows named pipes have no such
        // concern, so a plain temp dir is fine there.
        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new()
            .prefix("sotswrt-")
            .tempdir_in(runtime_base)
            .expect("runtime tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod runtime tempdir to 0700");
        }

        let socket_path = {
            #[cfg(windows)]
            {
                PathBuf::from(format!(r"\\.\pipe\sot-switchlat-{tag}-{}", std::process::id()))
            }
            #[cfg(unix)]
            {
                runtime_tmp.path().join(format!("wire-{tag}.sock"))
            }
        };
        let tmux_sock = runtime_tmp.path().join("tmux.sock");

        let daemon = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_STATE_HOST", format!("switchlat-{tag}"))
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .env("SOT_TMUX_SOCK", &tmux_sock)
            .env("SOT_TEST_SLOW_CONCEPT_READ_MS", SLOW_MS.to_string())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sotd");

        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            socket_path,
            tmux_sock,
            daemon,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Best-effort, bounded by nothing but a signal + a wait — mirrors
        // `capsule_workspaces.rs`'s own teardown philosophy without pulling
        // in its full bounded-async machinery for a single, short-lived
        // test: kill the daemon, then this env's own isolated tmux server
        // (never the developer's real one — a different socket entirely).
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.tmux_sock)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn try_connect(socket_path: &std::path::Path) -> Option<LocalStream> {
    let name = socket_path
        .to_str()
        .expect("socket path is valid UTF-8")
        .to_fs_name::<GenericFilePath>()
        .expect("interpret socket path as a local-socket name");
    tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(name))
        .await
        .ok()
        .and_then(Result::ok)
}

type Conn = tokio::io::BufReader<LocalStream>;

async fn poll_until_connected(socket_path: &std::path::Path) -> Conn {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Some(s) = try_connect(socket_path).await {
            return tokio::io::BufReader::new(s);
        }
        assert!(Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Proves the fix directly (switch-latency Phase 1): on ONE connection, fire
/// a slow `concept.read` (id 2, delayed `SLOW_MS` by the test-only knob) and,
/// immediately after — without waiting for its reply — a cheap `hello` (id
/// 3). Off-loop dispatch means id 3's reply must be OBSERVED before id 2's;
/// the pre-fix inline dispatch loop would have delayed id 3 behind id 2.
#[tokio::test]
async fn slow_concept_read_does_not_delay_a_later_cheap_reply_on_the_same_connection() {
    let env = Env::spawn("a");
    let mut conn = poll_until_connected(&env.socket_path).await;

    let hello = HelloReq {
        client_id: "switch-latency-test".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
    };
    let hello_payload = serde_json::to_value(&hello).unwrap();

    let body = async {
        // id 1: the real handshake — required before any other op is served.
        codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, hello_payload.clone()), None)
            .await
            .expect("write hello");
        loop {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
            if frame.id == 1 {
                assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
                break;
            }
        }

        // id 2: the SLOW request — no workspace/target setup needed, the
        // artificial delay fires before the handler ever looks at either.
        let slow_payload = serde_json::json!({ "target": "switch-latency/probe" });
        codec::write_frame(&mut conn, &Frame::req(2, op::CONCEPT_READ, slow_payload), None)
            .await
            .expect("write slow concept.read");

        // id 3: the CHEAP request — sent immediately after, on the same
        // connection, without waiting for id 2's reply.
        codec::write_frame(&mut conn, &Frame::req(3, op::HELLO, hello_payload), None)
            .await
            .expect("write cheap hello");

        // Read replies in wire order (skipping any evt fan-out, exactly as
        // a real client's steady-state loop does) until both {2, 3} have
        // answered, recording which was OBSERVED first.
        let mut order = Vec::new();
        while order.len() < 2 {
            let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read reply");
            if frame.kind == Kind::Evt || (frame.id != 2 && frame.id != 3) {
                continue;
            }
            order.push(frame.id);
        }
        order
    };

    let order = tokio::time::timeout(BOUND, body).await.expect("exchange did not finish within BOUND");

    assert_eq!(
        order,
        vec![3, 2],
        "the cheap hello (id 3) must be OBSERVED before the slow concept.read (id 2) — \
         off-loop dispatch means a slow request no longer head-of-line-blocks a later \
         cheap one on the same connection"
    );
}
