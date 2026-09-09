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
//!
//! Two more modules live in this file: `dead_kernel` (a confirmed-dead
//! kernel fails FAST with a typed reason, and startup ownership survives a
//! dropped connection without leaking a child process) and `pty_not_starved`
//! (pane traffic is never queued behind an off-loop kernel.request). Both use
//! a small fake `julia` shell-script stub (`write_fake_kernel`) driven by
//! env vars — no real Julia needed — gated `#[cfg(unix)]` where the stub
//! needs to be stateful (a POSIX-portable equivalent batch script isn't
//! practical; the supervisor logic under test is itself platform-agnostic
//! and covered on Windows by `cargo check --tests` compiling this file).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// Generous bound for the whole exchange (connect + hello + both requests +
/// the artificial delay), with headroom for a loaded CI runner and the
/// slow-boot case's real 8s wait. Not a precision timing assertion — just
/// the "don't hang forever" backstop every bounded test needs.
const BOUND: Duration = Duration::from_secs(30);

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
    /// One file per fake-kernel process spawned lands here (see
    /// `write_fake_kernel`'s `SOT_LANE_FAKE_JULIA_COUNTER_DIR`) — the only
    /// way these tests can count real child-process starts from outside the
    /// daemon. Always created, even for tests that never check it.
    spawn_marker_dir: PathBuf,
    daemon: Child,
}

impl Env {
    /// Spawn a real `sotd` with `SOT_TEST_SLOW_CONCEPT_READ_MS` set to
    /// `SLOW_MS` — the ONLY thing distinguishing this daemon from a
    /// production one.
    fn spawn(tag: &str) -> Self {
        Self::spawn_with(tag, None, &[], &[])
    }

    /// Same isolation recipe as `spawn`, plus the knobs the kernel tests
    /// need: an optional `julia_bin` override (a fake stub instead of a
    /// real Julia), a set of extra files to create under the project root
    /// (so `preview.get` has something to resolve a `node_id` against), and
    /// extra env vars forwarded to the daemon — which a fake-kernel stub
    /// then inherits transitively when the daemon spawns IT as a child
    /// (`write_fake_kernel`'s `SOT_LANE_FAKE_JULIA_*` knobs ride this).
    fn spawn_with(
        tag: &str,
        julia_bin: Option<&Path>,
        extra_files: &[(&str, &[u8])],
        extra_env: &[(&str, &str)],
    ) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("sot-switchlat-")
            .tempdir()
            .expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        for (name, contents) in extra_files {
            std::fs::write(project_root.join(name), contents).expect("write extra project file");
        }
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        let spawn_marker_dir = tmp.path().join("spawn-markers");
        std::fs::create_dir_all(&spawn_marker_dir).expect("mkdir spawn_marker_dir");

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

        let mut cmd = Command::new(sotd_exe());
        cmd.arg("--socket")
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
            .env("SOT_LANE_FAKE_JULIA_COUNTER_DIR", &spawn_marker_dir)
            .stdin(Stdio::null());
        if let Some(bin) = julia_bin {
            cmd.env("SOT_JULIA_BIN", bin);
        }
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let daemon = cmd.spawn().expect("spawn sotd");

        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            socket_path,
            tmux_sock,
            spawn_marker_dir,
            daemon,
        }
    }

    /// Count of fake-kernel process starts recorded so far — see
    /// `spawn_marker_dir`'s doc. Polled with a short retry window since a
    /// marker file write and this test's own read of it race benignly (the
    /// stub creates the file before doing anything else, but the OS write
    /// still needs to land).
    fn spawn_marker_count(&self) -> usize {
        std::fs::read_dir(&self.spawn_marker_dir)
            .map(|it| it.count())
            .unwrap_or(0)
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
        fe_handle: None,
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

/// Write the `hello` (id 1) frame and block until its reply lands, exactly
/// as every test below needs before any other op is served.
async fn do_hello(conn: &mut Conn) {
    let hello = HelloReq {
        client_id: "switch-latency-test".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        fe_handle: None,
    };
    codec::write_frame(conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
        .await
        .expect("write hello");
    loop {
        let (frame, _blob) = codec::read_frame(conn).await.expect("read hello reply");
        if frame.id == 1 {
            assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
            return;
        }
    }
}

/// Write a fake `julia` stub (POSIX `sh`) that speaks just enough of the
/// wire protocol to drive the real kernel supervisor through real
/// subprocess/timing behavior — no real Julia needed. Configured entirely
/// via env vars set on the DAEMON (`Env::spawn_with`'s `extra_env`), which
/// the daemon transitively passes to this script when it spawns it as the
/// kernel child (`tokio::process::Command` inherits the parent's
/// environment by default):
/// - `SOT_LANE_FAKE_JULIA_HELLO_DELAY_S` (default 0): sleep this long
///   before answering the FIRST request — always `kernel.hello`, id 0
///   (`kernel.rs`'s `run_one_generation` sends it first, always).
/// - `SOT_LANE_FAKE_JULIA_DIE_AFTER_N` (default: never): after ANSWERING
///   this many requests (hello counts as the first), exit WITHOUT replying
///   to the next one — `0` dies before ever answering hello (immediate
///   death); `1` answers hello then dies on the first real op (mid-request
///   death).
/// - `SOT_LANE_FAKE_JULIA_COUNTER_DIR` (always set by `Env::spawn_with`):
///   touch one uniquely-named file here per process started — the only way
///   these tests can count real child-process spawns from outside the
///   daemon (`Env::spawn_marker_count`).
///
/// One fixed script, reused by every test in both modules below — behavior
/// varies per test only through `extra_env`, never through the script text.
#[cfg(unix)]
fn write_fake_kernel(dir: &Path) -> PathBuf {
    let path = dir.join("fake-julia.sh");
    let script = r#"#!/bin/sh
if [ -n "$SOT_LANE_FAKE_JULIA_COUNTER_DIR" ]; then
    : > "$SOT_LANE_FAKE_JULIA_COUNTER_DIR/spawn-$$-$(date +%s%N 2>/dev/null || date +%s)"
fi
delay="${SOT_LANE_FAKE_JULIA_HELLO_DELAY_S:-0}"
die_after="${SOT_LANE_FAKE_JULIA_DIE_AFTER_N:-}"
served=0
first=1
while IFS= read -r line; do
    id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    op=$(printf '%s' "$line" | sed -n 's/.*"op":"\([^"]*\)".*/\1/p')
    served=$((served + 1))
    if [ -n "$die_after" ] && [ "$served" -gt "$die_after" ]; then
        exit 1
    fi
    if [ "$first" = 1 ] && [ "$delay" != "0" ]; then
        sleep "$delay"
    fi
    first=0
    if [ "$op" = "kernel.hello" ]; then
        printf '{"v":1,"id":%s,"kind":"res","op":"kernel.hello","payload":{"protocol":1,"version":"fake"}}\n' "$id"
    else
        printf '{"v":1,"id":%s,"kind":"res","op":"%s","payload":{"ok":true}}\n' "$id" "$op"
    fi
done
"#;
    std::fs::write(&path, script).expect("write fake kernel stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake kernel stub");
    path
}

/// One `kernel.request`, written then read back by id — used by every test
/// below that needs a single, timed round trip.
async fn one_kernel_request(conn: &mut Conn, id: u64, kernel_op: &str) -> (Duration, Frame) {
    let started = Instant::now();
    codec::write_frame(
        conn,
        &Frame::req(id, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": kernel_op})),
        None,
    )
    .await
    .expect("write kernel.request");
    let frame = loop {
        let (frame, _blob) = codec::read_frame(conn).await.expect("read kernel.request reply");
        if frame.id == id {
            break frame;
        }
    };
    (started.elapsed(), frame)
}

fn assert_kernel_unavailable(frame: &Frame, label: &str) {
    assert_eq!(
        frame.payload.get("code").and_then(|c| c.as_str()),
        Some("kernel_unavailable"),
        "{label}: unexpected payload: {:?}",
        frame.payload
    );
    let msg = frame.payload.get("error").and_then(|e| e.as_str()).unwrap_or_default();
    assert!(
        msg.contains("Julia kernel unavailable"),
        "{label}: expected the surfaced reason to name the kernel as unavailable, got: {msg:?}"
    );
}

/// A confirmed-dead kernel (`SOT_JULIA_BIN` pointed at a stub) must fail
/// FAST with a typed, surfaced reason — never queuing behind
/// `KERNEL_REQUEST_TIMEOUT` (10s). A kernel that is merely SLOW to start
/// must be given the full timeout and must succeed, never be killed. And
/// starting the kernel is the supervisor's own job, unaffected by a caller
/// disconnecting mid-startup.
#[cfg(unix)]
mod dead_kernel {
    use super::*;

    /// Bound for a single dead-kernel round trip. Generous for a loaded CI
    /// runner while still an order of magnitude under the field-observed
    /// ~3s (worst case the full 10s `KERNEL_REQUEST_TIMEOUT`) this fixes —
    /// a regression back to that stall fails this bound comfortably; the
    /// real-command proof times the production case precisely (sub-100ms).
    const DEAD_KERNEL_BOUND: Duration = Duration::from_secs(3);

    #[tokio::test]
    async fn preview_get_on_a_bounded_output_file_surfaces_kernel_unavailable_fast() {
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        // Contents don't matter — the daemon never gets far enough to
        // actually decode HDF5; the dead kernel fails before any plugin
        // logic runs. `.h5` only needs to be a BOUNDED-OUTPUT extension
        // (`is_bounded_output_plugin`) so `try_plugin_preview` surfaces the
        // reason instead of silently degrading to a bytes-level read
        // (which is the RIGHT behavior for, say, a `.jl` file, and
        // deliberately untouched by this fix).
        let env = Env::spawn_with(
            "deadkernel-preview",
            Some(&stub),
            &[("data.h5", b"not real hdf5")],
            &[("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "0")],
        );
        let mut conn = poll_until_connected(&env.socket_path).await;

        let body = async {
            do_hello(&mut conn).await;
            let started = Instant::now();
            codec::write_frame(
                &mut conn,
                &Frame::req(2, op::PREVIEW_GET, serde_json::json!({"node_id": "files:data.h5"})),
                None,
            )
            .await
            .expect("write preview.get");
            let (frame, _blob) = loop {
                let (frame, blob) = codec::read_frame(&mut conn).await.expect("read preview.get reply");
                if frame.id == 2 {
                    break (frame, blob);
                }
            };
            (started.elapsed(), frame)
        };

        let (elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");
        assert!(
            elapsed < DEAD_KERNEL_BOUND,
            "preview.get against a dead kernel took {elapsed:?}, expected well under {DEAD_KERNEL_BOUND:?}"
        );
        assert_kernel_unavailable(&frame, "preview.get");
    }

    /// Finding 9 / concurrency: exactly ONE spawn attempt per backoff
    /// window, no matter how many concurrent requests are asking — the
    /// supervisor is a single sequential loop, so ten pipelined
    /// `kernel.request`s against an always-dies stub must all share the
    /// SAME (one) spawn, and a request sent after the backoff floor elapses
    /// must trigger exactly one MORE.
    #[tokio::test]
    async fn concurrent_failures_spawn_exactly_one_child_and_backoff_throttles_the_next() {
        const N: u64 = 10;
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        let env = Env::spawn_with(
            "deadkernel-concurrency",
            Some(&stub),
            &[],
            &[("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "0")],
        );
        let mut conn = poll_until_connected(&env.socket_path).await;

        let body = async {
            do_hello(&mut conn).await;
            // Pipeline all N requests before reading any reply — real
            // concurrency at the wire level, since kernel.request runs
            // off-loop and these all race into `Kernel::request` together.
            for id in 2..(2 + N) {
                codec::write_frame(
                    &mut conn,
                    &Frame::req(id, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": "kernel.hello"})),
                    None,
                )
                .await
                .expect("write kernel.request");
            }
            let mut seen = 0;
            while seen < N {
                let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read reply");
                if frame.kind == Kind::Evt {
                    continue;
                }
                assert_kernel_unavailable(&frame, "concurrent kernel.request");
                seen += 1;
            }
        };
        tokio::time::timeout(BOUND, body).await.expect("exchange timed out");
        assert_eq!(
            env.spawn_marker_count(),
            1,
            "ten concurrent requests against a dead kernel must share exactly ONE spawn attempt"
        );

        // Past the 250ms respawn-backoff floor: exactly one MORE attempt.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (_elapsed, frame) = tokio::time::timeout(
            BOUND,
            one_kernel_request(&mut conn, 2 + N, "kernel.hello"),
        )
        .await
        .expect("exchange timed out");
        assert_kernel_unavailable(&frame, "post-backoff kernel.request");
        assert_eq!(
            env.spawn_marker_count(),
            2,
            "a request sent after the backoff window elapses must trigger exactly one more spawn"
        );
    }

    /// A child that answers `kernel.hello` and THEN dies before replying to
    /// the next request must deliver the typed unavailable reason to that
    /// SAME in-flight request (not a generic wire error) and record `Dead`
    /// before any other caller can see a stale `Running`.
    #[tokio::test]
    async fn child_that_dies_mid_request_delivers_kernel_unavailable_and_marks_dead() {
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        let env = Env::spawn_with(
            "deadkernel-midrequest",
            Some(&stub),
            &[],
            // Answers hello (request #1), dies on the very next one.
            &[("SOT_LANE_FAKE_JULIA_DIE_AFTER_N", "1")],
        );
        let mut conn = poll_until_connected(&env.socket_path).await;

        let body = async {
            do_hello(&mut conn).await;
            let first = one_kernel_request(&mut conn, 2, "kernel.hello").await;
            let second = one_kernel_request(&mut conn, 3, "kernel.hello").await;
            (first, second)
        };
        let ((_elapsed1, first), (_elapsed2, second)) =
            tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

        assert_kernel_unavailable(&first, "request that killed the kernel");
        // Sent immediately after, inside the backoff floor: must return the
        // CACHED Dead reason, not queue behind a second spawn attempt.
        assert_kernel_unavailable(&second, "request right after the kernel died");
        assert_eq!(
            env.spawn_marker_count(),
            1,
            "the second request must not have triggered a respawn yet (backoff floor)"
        );
    }

    /// The blocker this whole module exists to prove fixed: a slow but
    /// HEALTHY boot (well past the old, now-deleted 5s hello timeout) must
    /// succeed, not be killed. Real 8s subprocess sleep — no fake clock.
    #[tokio::test]
    async fn slow_but_healthy_boot_succeeds_within_the_request_timeout() {
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        let env = Env::spawn_with(
            "deadkernel-slowboot",
            Some(&stub),
            &[],
            &[("SOT_LANE_FAKE_JULIA_HELLO_DELAY_S", "8")],
        );
        let mut conn = poll_until_connected(&env.socket_path).await;

        let body = async {
            do_hello(&mut conn).await;
            one_kernel_request(&mut conn, 2, "kernel.hello").await
        };
        let (elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

        assert!(
            elapsed >= Duration::from_secs(7),
            "expected the real 8s hello delay to have elapsed, got {elapsed:?}"
        );
        assert!(
            frame.payload.get("code").and_then(|c| c.as_str()) != Some("kernel_unavailable"),
            "a slow-but-healthy boot must succeed, not report kernel_unavailable: {:?}",
            frame.payload
        );
        assert_eq!(env.spawn_marker_count(), 1, "a slow healthy boot must not be killed and retried");
    }

    /// Finding 3: a caller giving up (its connection dropped) mid-startup
    /// must not affect the supervisor at all — it keeps booting the SAME
    /// child, and a LATER caller on a fresh connection gets it once it's
    /// ready, with no second spawn (no orphan, no wasted respawn).
    #[tokio::test]
    async fn dropped_connection_during_startup_leaves_no_orphan() {
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        let env = Env::spawn_with(
            "deadkernel-cancel",
            Some(&stub),
            &[],
            &[("SOT_LANE_FAKE_JULIA_HELLO_DELAY_S", "2")],
        );

        let body = async {
            // Connection A: trigger startup, then vanish well before the 2s
            // hello resolves — never even reads a reply.
            let mut conn_a = poll_until_connected(&env.socket_path).await;
            do_hello(&mut conn_a).await;
            codec::write_frame(
                &mut conn_a,
                &Frame::req(2, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": "kernel.hello"})),
                None,
            )
            .await
            .expect("write kernel.request from connection A");
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(conn_a);
            // Give the daemon a moment to notice the close and unwind
            // connection A's own job set.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Connection B: a fresh request should still succeed once the
            // SAME startup (already ~400ms in) finishes.
            let mut conn_b = poll_until_connected(&env.socket_path).await;
            do_hello(&mut conn_b).await;
            one_kernel_request(&mut conn_b, 2, "kernel.hello").await
        };
        let (_elapsed, frame) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

        assert!(
            frame.payload.get("code").and_then(|c| c.as_str()) != Some("kernel_unavailable"),
            "connection B must see the kernel succeed, not report unavailable: {:?}",
            frame.payload
        );
        assert_eq!(
            env.spawn_marker_count(),
            1,
            "connection A's cancellation must not have caused an orphan or a second spawn"
        );
    }
}

/// Pane traffic must never wait behind kernel.request running off-loop.
/// `pty.screen` against an UNKNOWN `workspace_id` is the op used: it
/// answers inline (never touches `job_sem` — see `server.rs`'s
/// `op::PTY_SCREEN` arm) and fails fast with no tmux/shell-out at all,
/// which keeps this test's only variable the ONE thing under test (does the
/// connection's dispatch loop serve it promptly) rather than coupling to a
/// real tmux server's own startup timing.
#[cfg(unix)]
mod pty_not_starved {
    use super::*;

    /// The `pty.screen` reply must land well inside the real, several-
    /// second kernel-startup delay below — if the fix regressed and
    /// `kernel.request` went back to blocking this connection's dispatch
    /// loop inline, this reply would instead wait out the whole delay.
    const PTY_REPLY_BOUND: Duration = Duration::from_millis(500);

    #[tokio::test]
    async fn pty_screen_is_served_while_a_real_slow_kernel_request_is_pending() {
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = write_fake_kernel(stub_dir.path());
        let env = Env::spawn_with(
            "ptynotstarved",
            Some(&stub),
            &[],
            &[("SOT_LANE_FAKE_JULIA_HELLO_DELAY_S", "3")],
        );
        let mut conn = poll_until_connected(&env.socket_path).await;

        let body = async {
            do_hello(&mut conn).await;

            // id 2: a REAL slow kernel.request — the kernel takes 3s to
            // finish starting, so this won't reply for at least that long.
            codec::write_frame(
                &mut conn,
                &Frame::req(2, op::KERNEL_REQUEST, serde_json::json!({"kernel_op": "kernel.hello"})),
                None,
            )
            .await
            .expect("write slow kernel.request");

            // id 3: the pty op, sent immediately after — while the kernel
            // startup above is still pending.
            let started = Instant::now();
            let unknown_workspace = serde_json::json!({ "workspace_id": "does-not-exist" });
            codec::write_frame(&mut conn, &Frame::req(3, op::PTY_SCREEN, unknown_workspace), None)
                .await
                .expect("write pty.screen");

            // Read replies in wire order; the pty reply (id 3) must be
            // OBSERVED before the slow kernel.request (id 2) — this is the
            // ordering half of the head-of-line test above, now exercised
            // against the actual op this fix moved off-loop.
            let mut order = Vec::new();
            let mut pty_elapsed = None;
            while order.len() < 2 {
                let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read reply");
                if frame.kind == Kind::Evt || (frame.id != 2 && frame.id != 3) {
                    continue;
                }
                if frame.id == 3 {
                    pty_elapsed = Some(started.elapsed());
                }
                order.push(frame.id);
            }
            (order, pty_elapsed.expect("pty reply observed"))
        };

        let (order, pty_elapsed) = tokio::time::timeout(BOUND, body).await.expect("exchange timed out");

        assert_eq!(
            order,
            vec![3, 2],
            "the pty.screen reply (id 3) must be OBSERVED before the slow kernel.request (id 2) \
             — kernel.request running off-loop means it no longer head-of-line-blocks pane traffic \
             on the same connection"
        );
        assert!(
            pty_elapsed < PTY_REPLY_BOUND,
            "pty.screen took {pty_elapsed:?} while a real slow kernel.request was pending, \
             expected well under the 3s kernel-startup delay ({PTY_REPLY_BOUND:?} bound)"
        );
    }
}
