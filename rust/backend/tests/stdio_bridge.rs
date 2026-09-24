#![cfg(unix)]
//! `sotd stdio-bridge --label <label>` against real processes — the three
//! claims its callers depend on, each proved by running the real binary
//! with real pipes rather than by calling into `stdio_bridge::run`:
//!
//! 1. **Byte transparency.** An echo listener bound at the label's own
//!    endpoint gets back exactly what went in, and stdout carries exactly
//!    that and nothing else — no greeting, no trailing newline. The
//!    payload carries `\n`, `\r\n`, a lone `\r`, a NUL and a `0xff`,
//!    because this stream is newline-delimited and any translation of
//!    either newline form would corrupt a frame far from its cause.
//! 2. **It reaches a real daemon.** A hello frame written into the
//!    bridge's stdin comes back as that daemon's own reply.
//! 3. **A missing endpoint is a prompt, named failure** — nonzero at once,
//!    one line on stderr naming it, nothing at all on stdout. The value of
//!    the code is deliberately not asserted: there is one failure code,
//!    and the line is the diagnosis.
//!
//! Unix-gated because the echo listener below is a `UnixListener` bound at
//! the endpoint the daemon would own. The bridge itself is
//! platform-general (one connect per target, one copy loop); the Windows
//! half of the connect is `pipe_win`'s own already-tested connector.

mod support;

use std::io::{BufRead, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use support::sotd_exe;
#[cfg(target_os = "linux")]
use support::{Env, TEST_STATE_HOST};

/// Real-process tests share one CI runner; serialize them like every other
/// file in this crate that spawns a real `sotd`.
static SERIAL: Mutex<()> = Mutex::new(());

/// A private runtime dir of this suite's own, shared by every test here
/// and exported to every child. `XDG_RUNTIME_DIR` is the var that moves
/// `session_socket_path` (`SOT_RUNTIME_DIR` moves the supervisor/voyage
/// sockets, a different derivation), and moving it is the whole point:
/// without it every endpoint below would be bound in the REAL
/// `/run/user/<uid>/sot/sessions`, beside a live daemon's own socket.
/// One dir for the suite, not one per test, because this process resolves
/// it through its own env — so the value has to be settled before the
/// first derivation, and distinct labels keep the tests apart inside it.
static RUNTIME: OnceLock<tempfile::TempDir> = OnceLock::new();

fn runtime_root() -> &'static Path {
    let dir = RUNTIME.get_or_init(|| {
        // A literal `/tmp`, short prefix: every socket bound under this
        // eats into the 108-byte `sun_path` budget, and `$TMPDIR` can be
        // arbitrarily long (`support::Env` takes the same care).
        let tmp = tempfile::Builder::new().prefix("sotbr-").tempdir_in("/tmp").expect("runtime tempdir");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        // `private_xdg_runtime_dir` accepts it only when it is owner-only,
        // and this process must read it back through the same derivation
        // the children use.
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        tmp
    });
    dir.path()
}

const BOUND: Duration = Duration::from_secs(20);

/// Every newline shape the wire can carry, plus two bytes no text path
/// survives: a NUL and a non-UTF-8 `0xff`.
const PAYLOAD: &[u8] = b"one\ntwo\r\nthree\rfour\x00\xff\nfive\r\n";

fn spawn_bridge(label: &str) -> Child {
    Command::new(sotd_exe())
        .arg("stdio-bridge")
        .arg("--label")
        .arg(label)
        .env("XDG_RUNTIME_DIR", runtime_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sotd stdio-bridge")
}

/// Drains the bridge's stdout on its own thread and reports it in two
/// parts: first exactly `head` bytes, sent the moment they arrive (so the
/// caller can hold stdin open until it has the reply — closing stdin is
/// what ends the bridge), then everything up to EOF. The tail is what
/// proves stdout carried nothing of the bridge's own.
fn read_head_then_tail(mut out: std::process::ChildStdout, head: usize) -> Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut first = vec![0u8; head];
        if let Err(e) = out.read_exact(&mut first) {
            let _ = tx.send(Err(e));
            return;
        }
        let _ = tx.send(Ok(first));
        let mut rest = Vec::new();
        let _ = tx.send(out.read_to_end(&mut rest).map(|_| rest));
    });
    rx
}

fn next(rx: &Receiver<std::io::Result<Vec<u8>>>, what: &str) -> Vec<u8> {
    match rx.recv_timeout(BOUND) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => panic!("{what}: {e}"),
        Err(_) => panic!("{what}: nothing within {BOUND:?}"),
    }
}

#[test]
fn bytes_survive_both_newline_forms_and_stdout_carries_nothing_else() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let label = "bridge-echo";
    runtime_root();

    // The endpoint the bridge will resolve for itself — bound here by a
    // plain echo listener rather than a daemon, so the bytes coming back
    // are known exactly.
    let socket = sot_protocol::session_socket_path(label);
    std::fs::create_dir_all(socket.parent().expect("socket has a parent")).expect("mkdir sessions");
    let listener = UnixListener::bind(&socket).expect("bind the echo listener");
    let echo = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        loop {
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if conn.write_all(&buf[..n]).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut child = spawn_bridge(label);
    let mut stdin = child.stdin.take().expect("bridge stdin");
    let rx = read_head_then_tail(child.stdout.take().expect("bridge stdout"), PAYLOAD.len());
    stdin.write_all(PAYLOAD).expect("write the payload");
    stdin.flush().expect("flush the payload");

    let echoed = next(&rx, "the echoed payload");
    assert_eq!(echoed, PAYLOAD, "the bridge did not carry the bytes through unchanged");

    // Closing stdin is the caller going away: the bridge closes its end
    // and exits 0, and its stdout ends right where the payload did.
    drop(stdin);
    let tail = next(&rx, "stdout after the payload");
    assert!(tail.is_empty(), "the bridge wrote {} byte(s) of its own to stdout: {tail:?}", tail.len());

    let status = child.wait().expect("wait for the bridge");
    assert_eq!(status.code(), Some(0), "a caller hanging up is a clean exit, not a failure");
    let mut stderr = String::new();
    child.stderr.take().expect("bridge stderr").read_to_string(&mut stderr).expect("read stderr");
    assert!(stderr.is_empty(), "a clean run says nothing on stderr: {stderr:?}");
    echo.join().expect("echo listener");
}

#[test]
fn a_missing_endpoint_exits_promptly_with_one_stderr_line_and_no_stdout() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();

    let started = Instant::now();
    let out = spawn_bridge("nothing-listens-here").wait_with_output().expect("wait for the bridge");
    let elapsed = started.elapsed();

    assert!(!out.status.success(), "an endpoint that is not there is a failure");
    assert!(out.status.code().is_some(), "it exits, it is not killed by a signal: {:?}", out.status);
    assert!(out.stdout.is_empty(), "nothing may reach stdout, not even on the failure path: {:?}", out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "one line names the cause: {stderr:?}");
    assert!(stderr.contains("nothing-listens-here"), "the line names the endpoint it could not reach: {stderr:?}");
    // The connectors treat a missing endpoint as fatal on the first
    // attempt; this is the assertion that keeps it that way rather than
    // waiting out a connect bound (or a retry loop) to say so.
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?} to report an endpoint that is not there");
}

/// Linux-only for the same reason `status_integration.rs` is: it spawns a
/// real `sotd` and waits for it to bind. Everything the bridge itself does
/// is covered above on every Unix.
#[cfg(target_os = "linux")]
#[test]
fn a_hello_frame_reaches_a_real_daemon_and_its_reply_comes_back() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new("bridge-daemon");
    let label = "bridge-daemon";
    runtime_root();

    let hosts_toml = env._tmp.path().join("hosts.toml");
    std::fs::write(&hosts_toml, format!("hub = \"{TEST_STATE_HOST}\"\n\n[host.{TEST_STATE_HOST}]\ndaemon = true\n")).expect("write hosts.toml");
    let daemon = Command::new(sotd_exe())
        .arg("--label")
        .arg(label)
        .arg("--project-root")
        .arg(&env.daemon_project_root)
        .env("LOCALAPPDATA", &env.state_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("SOT_SELF_HOST", TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .env("XDG_RUNTIME_DIR", runtime_root())
        .env("SOT_HOSTS", &hosts_toml)
        // `SOT_SOCKET` outranks `--label` in the daemon's own arg parsing,
        // and a suite run from inside a Ship of Tools session inherits one
        // — which would point this "spawned daemon" at the live daemon's
        // socket instead of the temporary endpoint below.
        .env_remove("SOT_SOCKET")
        .env_remove("SOT_PROJECT_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sotd");
    env.daemon.borrow_mut().replace(daemon);

    let socket = sot_protocol::session_socket_path(label);
    std::fs::create_dir_all(socket.parent().expect("socket has a parent")).expect("mkdir sessions");
    let deadline = Instant::now() + BOUND;
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "sotd never bound {}", socket.display());
        std::thread::sleep(Duration::from_millis(50));
    }

    let hello = sot_protocol::HelloReq {
        client_id: "bridge-it".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some(TEST_STATE_HOST.to_string()),
        role: "fe".to_string(),
        instance: None,
        name: Some("bridge-it".to_string()),
    };
    // The wire's own framing, written by hand: one JSON envelope and one
    // `\n` (`codec::write_frame`), so nothing async is needed to prove a
    // real frame survives the trip.
    let frame = sot_protocol::Frame::req(1, sot_protocol::op::HELLO, serde_json::to_value(&hello).expect("hello serializes"));
    let mut line = serde_json::to_vec(&frame).expect("frame serializes");
    line.push(b'\n');

    let mut child = spawn_bridge(label);
    let mut stdin = child.stdin.take().expect("bridge stdin");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("bridge stdout"));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reply = Vec::new();
        let _ = tx.send(stdout.read_until(b'\n', &mut reply).map(|_| reply));
    });
    stdin.write_all(&line).expect("write the hello frame");
    stdin.flush().expect("flush the hello frame");

    let reply = next(&rx, "the daemon's hello reply");
    let parsed: sot_protocol::Frame = serde_json::from_slice(reply.strip_suffix(b"\n").unwrap_or(&reply))
        .unwrap_or_else(|e| panic!("what came back is not a frame ({e}) — the bridge added bytes of its own: {:?}", String::from_utf8_lossy(&reply)));
    assert_eq!(parsed.id, frame.id, "the reply answers the request that went in: {:?}", parsed.payload);
    assert!(parsed.payload.get("error").is_none(), "hello refused: {:?}", parsed.payload);

    drop(stdin);
    let status = child.wait().expect("wait for the bridge");
    assert_eq!(status.code(), Some(0), "the bridge exits cleanly when its caller hangs up");
}
