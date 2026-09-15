#![cfg(any(windows, target_os = "linux"))]
//! Half-open long-lived-role reaper (topology plan §F step 2): server-level
//! regression tests through the REAL wire protocol against a real `sotd`,
//! same posture as `active_frontend.rs` (no protocol doubles, no mocked
//! `handle_connection`) -- `Env`/`connect_and_hello`/`call`/`client_row`
//! copied from that file (its own header explains why this crate
//! duplicates rather than shares: each `tests/*.rs` binary is a separate
//! compilation unit, and `tests/support` is reserved for the two files
//! that already share a heavier capsule-process fixture).
//!
//! Before this lane a half-open `fe`/`bridge` connection (a closed laptop,
//! a killed bridge) was never reaped: no keepalive since 0.4.0, and
//! `WRITE_TIMEOUT` never fires for a tunnelled peer since its writes drain
//! into sshd. These four cases prove the fix -- OPT-IN BY PING (manager
//! compatibility fix, post-review): the deadline arms on a connection's
//! FIRST `ping`, never at hello, so an `fe`/`bridge` peer too old to send
//! one is left exactly as today, never reaped by this path, rather than
//! dropped every deadline forever:
//! - a connection that never pings survives indefinitely (never armed);
//! - one that pings once, then goes silent, is reaped within the
//!   deadline of that one ping (armed, then not bumped again);
//! - one that keeps pinging survives past what would otherwise have been
//!   that deadline (armed, kept bumped);
//! - an ordinary clean exit still reaps immediately, unaffected by any of
//!   the above.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};

/// Generous bound for a whole exchange -- not a precision timing assertion,
/// just the "don't hang forever" backstop every bounded test needs.
const BOUND: Duration = Duration::from_secs(20);

/// `SOT_TEST_PING_READ_DEADLINE_MS` this file spawns every reaper-exercising
/// daemon with -- short enough that the reaper tests run in well under
/// `BOUND`, generous enough that scheduling jitter on a loaded CI box never
/// produces a false positive/negative.
const TEST_DEADLINE_MS: u64 = 500;

/// One isolated `sotd`, rooted at a fresh temp project, with every path it
/// could touch OUTSIDE that tempdir redirected there too -- copied from
/// `active_frontend.rs`'s own `Env::spawn` (itself copied from
/// `switch_latency.rs`), trimmed to what this file needs plus the
/// `SOT_TEST_PING_READ_DEADLINE_MS` override this lane adds.
struct Env {
    _tmp: tempfile::TempDir,
    _runtime_tmp: tempfile::TempDir,
    socket_path: PathBuf,
    daemon: Child,
}

impl Env {
    /// `deadline_ms = None` leaves the daemon on its real 90s production
    /// deadline (used by the clean-exit test, to prove that path reaps
    /// immediately rather than merely "within 90s").
    fn spawn(tag: &str, deadline_ms: Option<u64>) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("sot-pingreap-")
            .tempdir()
            .expect("tempdir");
        let project_root = tmp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let state_root = tmp.path().join("state");
        std::fs::create_dir_all(&state_root).expect("mkdir state_root");
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");

        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new()
            .prefix("sotprwrt-")
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
                PathBuf::from(format!(r"\\.\pipe\sot-pingreap-{tag}-{}", std::process::id()))
            }
            #[cfg(unix)]
            {
                runtime_tmp.path().join(format!("wire-{tag}.sock"))
            }
        };
        let mut cmd = Command::new(sotd_exe());
        cmd.arg("--socket")
            .arg(&socket_path)
            .arg("--project-root")
            .arg(&project_root)
            .env("LOCALAPPDATA", &state_root)
            .env("XDG_STATE_HOME", &state_root)
            .env("XDG_CONFIG_HOME", &config_root)
            .env("SOT_SELF_HOST", format!("pingreap-{tag}"))
            .env("SOT_RUNTIME_DIR", runtime_tmp.path())
            .stdin(Stdio::null());
        if let Some(ms) = deadline_ms {
            cmd.env("SOT_TEST_PING_READ_DEADLINE_MS", ms.to_string());
        }
        let daemon = cmd.spawn().expect("spawn sotd");

        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            socket_path,
            daemon,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
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
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        if let Some(s) = try_connect(socket_path).await {
            return tokio::io::BufReader::new(s);
        }
        assert!(std::time::Instant::now() < deadline, "sotd's socket never accepted a connection");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Connect + hello declaring `role` (`"fe"` or `"bridge"`, topology plan §F
/// step 2's two long-lived roles) with a `name` (`<role>@<client_id>`).
/// Returns the connection and the next free request id (2 -- id 1 is hello).
async fn connect_and_hello(socket_path: &std::path::Path, client_id: &str, role: &str) -> (Conn, u64) {
    let mut conn = poll_until_connected(socket_path).await;
    let hello = HelloReq {
        client_id: client_id.to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some("test-host".to_string()),
        role: role.to_string(),
        instance: Some("test-instance".to_string()),
        name: Some(format!("{role}@{client_id}")),
    };
    codec::write_frame(&mut conn, &Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap()), None)
        .await
        .expect("write hello");
    let (frame, _blob) = codec::read_frame(&mut conn).await.expect("read hello reply");
    assert_eq!(frame.id, 1);
    assert!(frame.payload.get("error").is_none(), "hello refused: {:?}", frame.payload);
    (conn, 2)
}

/// Write a request and read replies (skipping evt frames and any other
/// request's reply) until `id`'s `res` frame is seen. Returns its payload.
async fn call(conn: &mut Conn, id: u64, opname: &str, payload: serde_json::Value) -> serde_json::Value {
    codec::write_frame(conn, &Frame::req(id, opname, payload), None)
        .await
        .unwrap_or_else(|e| panic!("write {opname} (id {id}): {e}"));
    let body = async {
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read reply");
            if frame.kind == Kind::Res && frame.id == id {
                return frame.payload;
            }
        }
    };
    tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("no reply to {opname} (id {id}) within BOUND"))
}

/// `version.query`'s roster entry for `client_id`, or `None` if absent.
fn client_row<'a>(version_res: &'a serde_json::Value, client_id: &str) -> Option<&'a serde_json::Value> {
    version_res
        .get("clients")?
        .as_array()?
        .iter()
        .find(|c| c.get("client_id").and_then(|v| v.as_str()) == Some(client_id))
}

/// Poll `version.query` on a fresh watcher connection until `client_id` is
/// gone from the roster, or `bound` elapses (panics on timeout).
async fn wait_until_absent(socket_path: &std::path::Path, client_id: &str, bound: Duration) {
    let (mut watcher, mut wid) = connect_and_hello(socket_path, "watcher", "fe").await;
    let deadline = std::time::Instant::now() + bound;
    loop {
        let v = call(&mut watcher, wid, op::VERSION_QUERY, serde_json::json!({})).await;
        wid += 1;
        if client_row(&v, client_id).is_none() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{client_id} was still in the roster after {bound:?}: {v}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The defect this lane fixes: an `fe` connection that stops sending
/// anything at all -- no `ping`, no other op -- since hello must be left
/// alone: the reaper is OPT-IN BY PING (manager compatibility fix,
/// post-review), armed on this connection's first `ping`, never at hello.
/// An `fe`/`bridge` peer too old to send `ping` (a frontend box or comm
/// bridge not yet converged from main) would otherwise be dropped every
/// `SOT_TEST_PING_READ_DEADLINE_MS` forever -- endless reconnect churn and
/// a message-loss window each cycle. Against the arm-at-hello version this
/// connection WAS reaped at the deadline; this test fails there (the
/// `version.query` below still finds it, proving it survived).
#[tokio::test]
async fn never_pinged_fe_connection_stays_connected_past_the_deadline() {
    let env = Env::spawn("neverping", Some(TEST_DEADLINE_MS));
    let (mut conn, mut id) = connect_and_hello(&env.socket_path, "neverping-fe", "fe").await;

    let body = async {
        // Wait well past what would have been the deadline had it armed
        // at hello.
        tokio::time::sleep(Duration::from_millis(TEST_DEADLINE_MS * 3)).await;

        // Still alive and still in the roster -- both checked on THIS
        // connection, not a fresh one, so a closed socket would show up
        // as a failed round trip rather than silently reconnecting.
        let v = call(&mut conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        id += 1;
        assert!(
            client_row(&v, "neverping-fe").is_some(),
            "a connection that never sent a ping must still be in the roster: {v}"
        );
    };
    tokio::time::timeout(BOUND, body)
        .await
        .expect("never-pinged connection did not survive within BOUND");
}

/// The arming half of the same fix: a connection that sends exactly ONE
/// `ping` -- after a delay LONGER than the deadline, so it could only
/// still be there because nothing reaped it before that ping -- and then
/// goes silent must be dropped within the deadline of that one ping.
/// Against the arm-at-hello version this connection was already reaped
/// during the initial wait, before it ever got to ping: the `call` below
/// would see a dead connection and fail there rather than at the final
/// `wait_until_absent`.
#[tokio::test]
async fn fe_connection_that_pings_once_then_goes_silent_is_reaped_within_the_deadline() {
    let env = Env::spawn("pingonce", Some(TEST_DEADLINE_MS));
    let (mut conn, id) = connect_and_hello(&env.socket_path, "pingonce-fe", "fe").await;

    let body = async {
        // Outlast what an arm-at-hello deadline would have been -- proves
        // (same as the sibling test above) that nothing reaped this
        // connection before it gets a chance to ping.
        tokio::time::sleep(Duration::from_millis(TEST_DEADLINE_MS * 2)).await;

        let res = call(&mut conn, id, op::PING, serde_json::json!({})).await;
        assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true), "ping refused: {res}");

        // Now go silent. This one ping must have armed the deadline --
        // the connection is reaped within it, same as any stale peer.
        let read = codec::read_frame(&mut conn).await;
        assert!(read.is_err(), "expected the daemon to close it after the deadline following the one ping, got: {read:?}");
        wait_until_absent(&env.socket_path, "pingonce-fe", Duration::from_secs(5)).await;
    };
    tokio::time::timeout(BOUND, body)
        .await
        .expect("ping-once-then-silent connection was not reaped within BOUND");
}

/// The other half: a connection that keeps sending `ping` every well under
/// the deadline must NOT be reaped, even well past what would otherwise
/// have been its deadline. Against today's code `ping` is an unknown op
/// (the generic catch-all), so this test's `call` would see an `error`
/// payload instead of `{ok:true}` and fail on the first assertion.
#[tokio::test]
async fn pinging_fe_connection_stays_in_the_roster() {
    let env = Env::spawn("pinger", Some(TEST_DEADLINE_MS));
    let (mut conn, mut id) = connect_and_hello(&env.socket_path, "pinger-fe", "fe").await;

    let body = async {
        // Ping at a fraction of the deadline, for well over 2x the
        // deadline in total -- if the reaper fired despite the pings,
        // the connection would already be gone by the final round.
        let rounds = 8;
        let ping_every = Duration::from_millis(TEST_DEADLINE_MS / 3);
        for _ in 0..rounds {
            tokio::time::sleep(ping_every).await;
            let res = call(&mut conn, id, op::PING, serde_json::json!({})).await;
            id += 1;
            assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true), "ping refused: {res}");
        }

        let v = call(&mut conn, id, op::VERSION_QUERY, serde_json::json!({})).await;
        assert!(
            client_row(&v, "pinger-fe").is_some(),
            "a connection that kept pinging must still be in the roster: {v}"
        );
    };
    tokio::time::timeout(BOUND, body)
        .await
        .expect("pinging connection exchange did not finish within BOUND");
}

/// Regression guard: a clean exit (the client closes its own socket) must
/// still be reaped immediately -- unaffected by the new deadline
/// machinery. Spawned with NO deadline override (the real 90s production
/// value), so "immediately" here can only mean the pre-existing
/// `ClientGuard::drop`-on-EOF path, never the new timer.
#[tokio::test]
async fn clean_exit_still_reaps_immediately() {
    let env = Env::spawn("cleanexit", None);
    let (conn, _id) = connect_and_hello(&env.socket_path, "cleanexit-fe", "fe").await;
    drop(conn);

    let body = wait_until_absent(&env.socket_path, "cleanexit-fe", Duration::from_secs(3));
    tokio::time::timeout(BOUND, body)
        .await
        .expect("a clean exit must reap well within BOUND, long before any 90s deadline");
}
