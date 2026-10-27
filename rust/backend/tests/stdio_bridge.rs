#![cfg(unix)]
//! `sotd stdio-bridge [--host <host> | --endpoint <unix:PATH|pipe:PATH>]`
//! against real processes — the claims
//! its callers depend on, each proved by running the real binary with real
//! pipes rather than by calling into `stdio_bridge::run`:
//!
//! 1. **Byte transparency, no argument.** An echo listener bound at THIS
//!    box's own endpoint (`session_socket_path(local_daemon_label())`)
//!    gets back exactly what went in, and stdout carries exactly that and
//!    nothing else — no greeting, no trailing newline. The payload carries
//!    `\n`, `\r\n`, a lone `\r`, a NUL and a `0xff`, because this stream is
//!    newline-delimited and any translation of either newline form would
//!    corrupt a frame far from its cause.
//! 2. **`--host <host>` reaches the hub's relay socket for that host**, a
//!    different derivation (`topology::relay_socket_path`) than the
//!    no-argument form — proved by the same byte-transparency round trip.
//! 3. **An unrecognised form is a usage error** — nonzero, one line on stderr
//!    naming all supported forms, nothing on stdout.
//! 4. **A missing endpoint is a prompt, named failure** — nonzero at once,
//!    one line on stderr naming it, nothing at all on stdout. The value of
//!    the code is deliberately not asserted: there is one failure code,
//!    and the line is the diagnosis.
//! 5. **It reaches a real daemon**, no argument on either side: the daemon
//!    started at `local_daemon_label()` and the bridge started with no
//!    argument reach the same socket, because that is the whole point of
//!    dropping a caller-supplied label.
//! 6. **A `link.hello` first makes it a window's router** (topology's guarantees 12, 14 and 15): each channel the
//!    link opens is one more connection to the same endpoint, admitted by the daemon as any connection; the link's
//!    end ends every one; a plain first frame is still forwarded byte for byte, and `--host` never routes.
//!
//! Unix-gated because the echo listener below is a `UnixListener` bound at
//! the endpoint the daemon would own. The bridge itself is
//! platform-general (one connect per target, one copy loop); the Windows
//! half of the connect is `pipe_win`'s own already-tested connector.

mod support;

use std::io::{BufRead, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Stdio};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use support::{Env, TEST_STATE_HOST};

/// Real-process tests share one CI runner; serialize them like every other
/// file in this crate that spawns a real `sotd`. Serializing also gives
/// every test exclusive use of the fixed no-argument socket path below —
/// there is no longer a per-test label to keep them apart there.
static SERIAL: Mutex<()> = Mutex::new(());

/// A private runtime dir of this suite's own, shared by every test here
/// and exported to every child. `XDG_RUNTIME_DIR` is the var that moves
/// `session_socket_path` (`SOT_RUNTIME_DIR` moves the supervisor/voyage
/// sockets, a different derivation), and moving it is the whole point:
/// without it every endpoint below would be bound in the REAL
/// `/run/user/<uid>/sot/sessions`, beside a live daemon's own socket.
/// One dir for the suite, not one per test: the no-argument form always
/// resolves the SAME path (`local_daemon_label()` takes no override), so
/// tests share it and rely on `SERIAL` plus the daemon's own stale-socket
/// cleanup at bind time rather than on distinct labels.
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
        // Every endpoint here is a sibling in one sessions dir; derive it
        // rather than spelling the layout out a second time.
        let sessions =
            sot_topology::endpoint::session_socket_path(sot_topology::endpoint::local_daemon_label()).parent().expect("a socket has a parent").to_path_buf();
        create_private_under(tmp.path(), &sessions);
        tmp
    });
    dir.path()
}

/// Creates `dir` and every component of it below `root` with an explicit
/// `0700`, mirroring `sot_platform::host::paths::secure_socket_dir` — the check the daemon
/// applies to its own socket's parents, and BAILS on. A plain
/// `create_dir_all` here would leave whatever the ambient umask allows:
/// `0700` on a box with a `0077` umask, `0755` on a CI runner's `0022`,
/// where the daemon then refuses to bind and the wait below has nothing
/// to wait for.
fn create_private_under(root: &Path, dir: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    let rel = dir.strip_prefix(root).expect("the endpoint lives under this suite's runtime root");
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        match std::fs::DirBuilder::new().mode(0o700).create(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("mkdir {}: {e}", cur.display()),
        }
    }
}

const BOUND: Duration = Duration::from_secs(20);

/// Every newline shape the wire can carry, plus two bytes no text path
/// survives: a NUL and a non-UTF-8 `0xff`.
const PAYLOAD: &[u8] = b"one\ntwo\r\nthree\rfour\x00\xff\nfive\r\n";

fn spawn_bridge(args: &[&str]) -> Child {
    let mut cmd = support::sotd_command();
    cmd.arg("stdio-bridge");
    for a in args {
        cmd.arg(a);
    }
    cmd.env("XDG_RUNTIME_DIR", runtime_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sotd stdio-bridge")
}

/// Runs the bridge with `args` and its input closed at once (as `wait_with_output` did), and waits for it within
/// `BOUND`: `sot_platform::test_isolated`'s drain reads its output as it comes, and a bridge that never exits, or output a
/// descendant still holds, fails the test at the bound instead of hanging the suite.
fn run_bridge(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let mut child = spawn_bridge(args);
    drop(child.stdin.take());
    sot_platform::test_isolated::drain(child).wait_within(BOUND)
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

/// One byte-transparency round trip against a listener already bound at
/// `socket`, driving the bridge with `args`. Shared by the no-argument
/// case and the `--host` case, which differ only in which path the
/// listener binds and which args reach it.
fn round_trip(socket: &Path, args: &[&str]) {
    // Best-effort stale-unlink, the same thing `sotd`'s own bind does: the
    // no-argument path is fixed (`local_daemon_label()` takes no
    // override), so a killed daemon from an earlier test in this suite can
    // leave the dirent behind and `bind` would otherwise fail with
    // `AddrInUse` on a file nothing is listening on.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).expect("bind the echo listener");
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

    let mut child = spawn_bridge(args);
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

    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    let said = format!("{status}; stderr {stderr:?}");
    assert_eq!(status.code(), Some(0), "a caller hanging up is a clean exit, not a failure: {said}");
    assert!(stderr.is_empty(), "a clean run says nothing on stderr: {said}");
    echo.join().expect("echo listener");
}

#[test]
fn no_argument_reaches_this_boxs_own_endpoint() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    let socket = sot_topology::endpoint::session_socket_path(sot_topology::endpoint::local_daemon_label());
    round_trip(&socket, &[]);
}

#[test]
fn dash_dash_host_reaches_the_hubs_relay_socket_for_that_host() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    let socket = sot_topology::relay_socket_path("bridge-host");
    round_trip(&socket, &["--host", "bridge-host"]);
}

/// ADR 0049, User isolation: a local endpoint in this account's private folder is reached byte-for-byte.
#[test]
fn dash_dash_endpoint_reaches_a_socket_in_a_private_folder() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let socket = runtime_root().join("endpoint.sock");
    let endpoint = format!("unix:{}", socket.display());
    round_trip(&socket, &["--endpoint", &endpoint]);
}

/// ADR 0049, User isolation: `--endpoint` to a socket another account listens on is refused, and the listener gets none
/// of the bridge's input. A zero count is evidence of no write on Linux only: macOS may refuse the accept of a client
/// that has closed.
#[test]
fn dash_dash_endpoint_refuses_a_socket_another_account_listens_on() {
    if !sot_platform::test_isolated::run_isolated(
        "dash_dash_endpoint_refuses_a_socket_another_account_listens_on",
    ) {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(foreign) = sot_platform::test_foreign::ForeignListener::start(false) else {
        return;
    };
    let endpoint = format!("unix:{}", foreign.path.display());
    let mut child = spawn_bridge(&["--endpoint", &endpoint]);
    let mut stdin = child.stdin.take().expect("bridge stdin");
    // A bridge that has already exited refuses the write; its stderr says why.
    let _ = stdin.write_all(b"ping\n").and_then(|()| stdin.flush());
    drop(stdin);
    let (status, stdout, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert_eq!(
        foreign.finish(),
        0,
        "the bridge sent another account's listener bytes"
    );
    assert!(
        !status.success(),
        "a socket another account listens on is refused: {said}"
    );
    assert!(
        stderr.contains("another OS account listens on this socket"),
        "{said}"
    );
    assert!(stdout.is_empty(), "a refusal writes nothing to stdout: {said}");
}

/// ADR 0049, User isolation: the bridge returns within `CONNECT_BOUND` plus 5 s of slack against a socket whose backlog
/// another OS account has filled, and its error is not the account refusal, so the backlog was full.
#[test]
fn a_full_foreign_backlog_ends_the_bridge_within_its_bound() {
    if !sot_platform::test_isolated::run_isolated(
        "a_full_foreign_backlog_ends_the_bridge_within_its_bound",
    ) {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(foreign) = sot_platform::test_foreign::ForeignListener::start(true) else {
        return;
    };
    let endpoint = format!("unix:{}", foreign.path.display());
    let mut child = spawn_bridge(&["--endpoint", &endpoint]);
    drop(child.stdin.take());
    let (status, stdout, stderr) = sot_platform::test_isolated::drain(child)
        .wait_within(sot_platform::transport::CONNECT_BOUND + Duration::from_secs(5));
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(
        !status.success(),
        "the bridge connected through a full backlog: {said}"
    );
    assert!(
        !stderr.contains("not connecting"),
        "the connect went through, so the backlog was not full: {said}"
    );
    assert_eq!(
        foreign.finish(),
        0,
        "the bridge sent another account's listener bytes"
    );
}

/// ADR 0049, User isolation: the endpoint form accepts only this platform's local scheme.
#[test]
fn dash_dash_endpoint_refuses_the_other_platforms_scheme() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (status, stdout, stderr) = run_bridge(&["--endpoint", "pipe:x"]);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(!status.success(), "the other platform's scheme is refused: {said}");
    assert_eq!(stderr.lines().count(), 1, "one line names the refused endpoint: {said}");
    assert!(stderr.contains("pipe:x"), "{said}");
    assert!(stdout.is_empty(), "a refusal writes nothing to stdout: {said}");
}

/// The relay socket's path is derived from the host name, so a name outside the plain host-name grammar is refused
/// before any path is built (topology's contract, guarantee 9).
#[test]
fn stdio_bridge_refuses_a_host_outside_the_grammar() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    let (status, stdout, stderr) = run_bridge(&["--host", "../escape"]);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(!status.success(), "a host outside the grammar is refused: {said}");
    assert!(stdout.is_empty(), "a refusal writes nothing to stdout: {said}");
    assert_eq!(stderr.lines().count(), 1, "one line names the refusal: {said}");
    assert!(stderr.contains("not a plain host name"), "the line names the grammar: {said}");
}

#[test]
fn a_third_form_is_a_usage_error() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    // The dropped flag itself, kept as the usage-error case: it is exactly
    // the argument pattern this change removes.
    let (status, stdout, stderr) = run_bridge(&["--label", "sot"]);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");
    assert!(!status.success(), "an unrecognised form is a failure: {said}");
    assert!(stdout.is_empty(), "nothing may reach stdout on the usage-error path: {said}");
    assert_eq!(stderr.lines().count(), 1, "one line names the usage: {said}");
    assert!(
        stderr.contains("Usage: sotd stdio-bridge [--host <host> | --endpoint <unix:PATH|pipe:PATH>]"),
        "{said}"
    );
}

#[test]
fn a_missing_endpoint_exits_promptly_with_one_stderr_line_and_no_stdout() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();

    // `--host` with a name nothing binds: the relay-socket derivation puts
    // the name straight into the path, so the diagnosis names it without
    // this test needing a caller-supplied label the flag no longer has.
    let (status, stdout, stderr) = run_bridge(&["--host", "nothing-listens-here"]);
    let said = format!("{status}; stdout {stdout:?}; stderr {stderr:?}");

    assert!(!status.success(), "an endpoint that is not there is a failure: {said}");
    assert!(status.code().is_some(), "it exits, it is not killed by a signal: {said}");
    assert!(stdout.is_empty(), "nothing may reach stdout, not even on the failure path: {said}");
    assert_eq!(stderr.lines().count(), 1, "one line names the cause: {said}");
    assert!(stderr.contains("nothing-listens-here"), "the line names the endpoint it could not reach: {said}");
    // The fatal first attempt names op `connect`; the bounded retry names
    // `connect(bounded retry)`.
    assert!(
        stderr.contains(": connect: ") && !stderr.contains("bounded retry"),
        "a missing endpoint is fatal on the first attempt, not after the connect bound: {said}"
    );
}

/// Whatever the daemon managed to say before giving up.
#[cfg(target_os = "linux")]
fn daemon_said(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| format!("<its stderr is unreadable: {e}>")).trim().to_string()
}

/// Linux-only: it spawns a real `sotd` and waits for it to bind. Everything the bridge itself does
/// is covered above on every Unix.
#[cfg(target_os = "linux")]
#[test]
fn a_hello_frame_reaches_a_real_daemon_and_its_reply_comes_back() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new("bridge-daemon");
    // The daemon MUST run at this box's own label — a no-argument bridge
    // never accepts a caller-supplied one, so this is the only label that
    // reaches it, exactly as the hub's own relay unit runs it.
    let label = sot_topology::endpoint::local_daemon_label();
    runtime_root();

    let hosts_toml = env._tmp.path().join("hosts.toml");
    std::fs::write(&hosts_toml, format!("hub = \"{TEST_STATE_HOST}\"\n\n[host.{TEST_STATE_HOST}]\ndaemon = true\n")).expect("write hosts.toml");
    // The daemon's own stderr, kept rather than discarded: when it
    // refuses to start there is one line saying so, and the wait below
    // has no other way to report why nothing ever bound.
    let daemon_stderr = env._tmp.path().join("sotd.stderr");
    let daemon = support::sotd_command()
        .arg("--label")
        .arg(label)
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
        .env("XDG_RUNTIME_DIR", runtime_root())
        .env("SOT_HOSTS", &hosts_toml)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(&daemon_stderr).expect("create the daemon's stderr file")))
        .spawn()
        .expect("spawn sotd");
    env.daemon.borrow_mut().replace(daemon);

    let socket = sot_topology::endpoint::session_socket_path(label);
    let deadline = Instant::now() + BOUND;
    while UnixStream::connect(&socket).is_err() {
        // A daemon that has exited will never bind, however slow the
        // runner is — so this wait ends on the condition (bound, or
        // gone), and BOUND covers only the honest case of one that is
        // still starting. Sleeping out twenty seconds to then report
        // nothing is what hid the cause of this test's first CI failure.
        let exited = env.daemon.borrow_mut().as_mut().expect("the daemon is tracked").try_wait().expect("try_wait the daemon");
        if let Some(status) = exited {
            panic!("sotd exited {status} without binding {}: {}", socket.display(), daemon_said(&daemon_stderr));
        }
        assert!(
            Instant::now() < deadline,
            "sotd is still running but never bound {} within {BOUND:?}: {}",
            socket.display(),
            daemon_said(&daemon_stderr)
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let hello = sot_protocol::HelloReq {
        name: Some("bridge-it".to_string()),
        ..sot_protocol::HelloReq::this_process("bridge-it", "fe", Some(TEST_STATE_HOST.to_string())).expect("this process's account")
    };
    // The wire's own framing, written by hand: one JSON envelope and one
    // `\n` (`codec::write_frame`), so nothing async is needed to prove a
    // real frame survives the trip.
    let frame = sot_protocol::Frame::req(1, sot_protocol::op::HELLO, serde_json::to_value(&hello).expect("hello serializes"));
    let mut line = serde_json::to_vec(&frame).expect("frame serializes");
    line.push(b'\n');

    let mut child = spawn_bridge(&[]);
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
    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    assert_eq!(status.code(), Some(0), "the bridge exits cleanly when its caller hangs up: {stderr:?}");
}

/// A lease sent through the bridge is the bridge's own connection: the
/// daemon reads the bridge process at accept, which never matches the
/// claim, so a bridged or relayed window never counts as a window here,
/// and its end never shuts the daemon down (#2).
#[cfg(target_os = "linux")]
#[test]
fn lease_through_stdio_bridge_refused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new("bridge-lease");
    let label = sot_topology::endpoint::local_daemon_label();
    runtime_root();
    let hosts_toml = env._tmp.path().join("hosts.toml");
    std::fs::write(&hosts_toml, format!("hub = \"{TEST_STATE_HOST}\"\n\n[host.{TEST_STATE_HOST}]\ndaemon = true\n")).expect("write hosts.toml");
    let daemon_stderr = env._tmp.path().join("sotd.stderr");
    let daemon = support::sotd_command()
        .arg("--label")
        .arg(label)
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
        .env("XDG_RUNTIME_DIR", runtime_root())
        .env("SOT_HOSTS", &hosts_toml)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(&daemon_stderr).expect("create the daemon's stderr file")))
        .spawn()
        .expect("spawn sotd");
    env.daemon.borrow_mut().replace(daemon);
    let socket = sot_topology::endpoint::session_socket_path(label);
    let deadline = Instant::now() + BOUND;
    while UnixStream::connect(&socket).is_err() {
        let exited = env.daemon.borrow_mut().as_mut().expect("the daemon is tracked").try_wait().expect("try_wait the daemon");
        if let Some(status) = exited {
            panic!("sotd exited {status} without binding {}: {}", socket.display(), daemon_said(&daemon_stderr));
        }
        assert!(Instant::now() < deadline, "sotd never bound {}: {}", socket.display(), daemon_said(&daemon_stderr));
        std::thread::sleep(Duration::from_millis(50));
    }

    // The claim is this test process's own identity; the peer the daemon
    // reads is the bridge.
    let who = sot_platform::identity::challenge::self_identity().expect("this process's identity");
    let req = sot_protocol::ops::FeLeaseReq { boot: who.boot, pid: who.pid, created: who.created, token: None };
    let frame = sot_protocol::Frame::req(1, sot_protocol::op::FE_LEASE, serde_json::to_value(&req).expect("fe.lease serializes"));
    // A handoff hello and the lease in one write, as the window writes them through the bridge; the hello's
    // reply comes first.
    let hello = sot_protocol::HelloReq::this_process("bridge-it-lease", sot_protocol::HANDOFF_ROLE, Some(TEST_STATE_HOST.to_string()))
        .expect("this process's account");
    let hello = sot_protocol::Frame::req(0, sot_protocol::op::HELLO, serde_json::to_value(&hello).expect("hello serializes"));
    let mut line = serde_json::to_vec(&hello).expect("frame serializes");
    line.push(b'\n');
    line.extend_from_slice(&serde_json::to_vec(&frame).expect("frame serializes"));
    line.push(b'\n');

    let mut child = spawn_bridge(&[]);
    let mut stdin = child.stdin.take().expect("bridge stdin");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("bridge stdout"));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // The hello's reply, then the lease's.
        let mut hello_reply = Vec::new();
        let _ = stdout.read_until(b'\n', &mut hello_reply);
        let mut reply = Vec::new();
        let _ = tx.send(stdout.read_until(b'\n', &mut reply).map(|_| reply));
    });
    stdin.write_all(&line).expect("write the hello and fe.lease");
    stdin.flush().expect("flush the hello and fe.lease");
    let reply = next(&rx, "the daemon's lease answer");
    let parsed: sot_protocol::Frame = serde_json::from_slice(reply.strip_suffix(b"\n").unwrap_or(&reply)).expect("a frame");
    assert_eq!(parsed.payload["outcome"], "foreign", "a bridged lease must be foreign: {:?}", parsed.payload);

    drop(stdin);
    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    assert_eq!(status.code(), Some(0), "the bridge exits cleanly after a bridged lease: {stderr:?}");
    std::thread::sleep(Duration::from_secs(2));
    let exited = env.daemon.borrow_mut().as_mut().expect("the daemon is tracked").try_wait().expect("try_wait the daemon");
    assert!(exited.is_none(), "the bridged lease's end shut the daemon down: {exited:?}: {}", daemon_said(&daemon_stderr));
    assert!(UnixStream::connect(&socket).is_ok(), "the daemon stopped answering after a bridged lease");
}

// --- The router: a window's link whose far end is this bridge (topology's guarantees 12, 14 and 15). ---

/// A listener that echoes every connection it takes, each on a thread of its own, counting the connections and the
/// ends it saw.
struct EchoListener {
    accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl EchoListener {
    fn bind(socket: &Path) -> Self {
        use std::sync::atomic::Ordering;
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).expect("bind the echo listener");
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (taken, ended) = (accepts.clone(), ends.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                taken.fetch_add(1, Ordering::SeqCst);
                let ended = ended.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match conn.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if conn.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    ended.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        Self { accepts, ends }
    }

    fn accepts(&self) -> usize {
        self.accepts.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn ends(&self) -> usize {
        self.ends.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The bridge with `args`, its stdio piped, and a window's link over them; the child is killed on drop.
fn bridge_link(rt: &tokio::runtime::Runtime, args: &[&str]) -> (tokio::process::Child, sot_topology::link::Link) {
    let mut cmd = support::sotd_command();
    cmd.arg("stdio-bridge").args(args).env("XDG_RUNTIME_DIR", runtime_root());
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    rt.block_on(async {
        let mut child = tokio::process::Command::from(cmd).kill_on_drop(true).spawn().expect("spawn sotd stdio-bridge");
        let (stdout, stdin) = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let link = tokio::time::timeout(BOUND, sot_topology::link::Link::over(stdout, stdin))
            .await
            .expect("the link's hello within the bound")
            .expect("the bridge answers link.hello as a router");
        (child, link)
    })
}

/// Writes `bytes` on `channel` and reads the same count back.
async fn channel_echo(channel: &mut sot_topology::link::Channel, bytes: &[u8]) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    channel.write_all(bytes).await.expect("write on the channel");
    channel.flush().await.expect("flush the channel");
    let mut back = vec![0u8; bytes.len()];
    tokio::time::timeout(BOUND, channel.read_exact(&mut back)).await.expect("the echo within the bound").expect("read the echo");
    back
}

fn wait_for(what: &str, bound: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + bound;
    while !done() {
        assert!(Instant::now() < deadline, "{what}: not within {bound:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Topology 12: a `link.hello` first makes the bridge a router: its connection is channel 0, and each channel the
/// window opens is one more connection to the same endpoint, carrying its own bytes.
#[test]
fn a_link_hello_makes_the_bridge_a_router() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let socket = runtime_root().join("link-echo.sock");
    let echo = EchoListener::bind(&socket);
    let endpoint = format!("unix:{}", socket.display());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (_child, mut link) = bridge_link(&rt, &["--endpoint", &endpoint]);
    assert!(!link.build().is_empty(), "the router names its build");
    let handle = sot_topology::link::LinkHandle::default();
    handle.set_up(&link);
    rt.block_on(async {
        let mut control = link.take_control().expect("channel 0");
        assert_eq!(channel_echo(&mut control, b"channel 0\n").await, b"channel 0\n");
        let mut row = handle.open().await.expect("a row's channel");
        let mut page = handle.open().await.expect("a page's channel");
        assert_eq!(channel_echo(&mut row, PAYLOAD).await, PAYLOAD, "a channel carries any byte unchanged");
        assert_eq!(channel_echo(&mut page, b"GET / HTTP/1.1\r\n\r\n").await, b"GET / HTTP/1.1\r\n\r\n");
    });
    assert_eq!(echo.accepts(), 3, "channel 0 and one connection per opened channel");
}

/// Topology 14: a dropped link ends its router, which ends every connection it made, within 2 s: the window's end of
/// input reaches the bridge, the bridge exits 0, and the endpoint sees each connection end.
#[test]
fn a_dropped_link_ends_its_router_and_every_daemon_connection() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let socket = runtime_root().join("link-drop.sock");
    let echo = EchoListener::bind(&socket);
    let endpoint = format!("unix:{}", socket.display());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (mut child, mut link) = bridge_link(&rt, &["--endpoint", &endpoint]);
    let handle = sot_topology::link::LinkHandle::default();
    handle.set_up(&link);
    let channels = rt.block_on(async {
        let mut control = link.take_control().unwrap();
        assert_eq!(channel_echo(&mut control, b"up").await, b"up");
        let mut held = vec![control];
        for _ in 0..3 {
            let mut channel = handle.open().await.expect("open");
            assert_eq!(channel_echo(&mut channel, b"up").await, b"up");
            held.push(channel);
        }
        held
    });
    assert_eq!((echo.accepts(), echo.ends()), (4, 0));
    let dropped = Instant::now();
    drop(link);
    let exited = rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), child.wait()).await });
    let status = exited.expect("the bridge exits within 2 s of the link's drop").expect("wait for the bridge");
    assert_eq!(status.code(), Some(0), "a closed window is a clean end");
    wait_for("every connection the router made ends", Duration::from_secs(2), || echo.ends() == 4);
    println!("link drop: the router and its 4 connections ended {:?} after the drop", dropped.elapsed());
    drop(channels);
}

/// Topology 12: a plain first frame is forwarded byte for byte: a hello line, then more bytes, all echoed exactly.
#[test]
fn a_plain_first_frame_is_forwarded_byte_for_byte() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    let hello = sot_protocol::HelloReq::this_process("bridge-plain", "fe", Some("plain-host".to_string())).expect("this process's account");
    let frame = sot_protocol::Frame::req(1, sot_protocol::op::HELLO, serde_json::to_value(&hello).unwrap());
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    bytes.extend_from_slice(PAYLOAD);
    let socket = runtime_root().join("plain.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind");
    let echo = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        while let Ok(n) = conn.read(&mut buf) {
            if n == 0 || conn.write_all(&buf[..n]).is_err() {
                return;
            }
        }
    });
    let endpoint = format!("unix:{}", socket.display());
    let mut child = spawn_bridge(&["--endpoint", &endpoint]);
    let mut stdin = child.stdin.take().unwrap();
    let rx = read_head_then_tail(child.stdout.take().unwrap(), bytes.len());
    stdin.write_all(&bytes).unwrap();
    stdin.flush().unwrap();
    assert_eq!(next(&rx, "the echoed hello and bytes"), bytes, "a plain first frame and what follows it, unchanged");
    drop(stdin);
    assert!(next(&rx, "stdout after").is_empty());
    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    assert_eq!(status.code(), Some(0), "{stderr}");
    echo.join().unwrap();
}

/// Topology 12: `--host` never routes: its endpoint is a relay socket, where each connection is a login, so a
/// `link.hello` goes through it unchanged to the far bridge, which routes.
#[test]
fn dash_dash_host_carries_a_link_hello_unchanged() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    runtime_root();
    let socket = sot_topology::relay_socket_path("link-host");
    let hello = sot_protocol::Frame::req(1, sot_protocol::op::LINK_HELLO, serde_json::json!({ "link": 1 }));
    let mut line = serde_json::to_vec(&hello).unwrap();
    line.push(b'\n');
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind the relay stand-in");
    let echo = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        while let Ok(n) = conn.read(&mut buf) {
            if n == 0 || conn.write_all(&buf[..n]).is_err() {
                return;
            }
        }
    });
    let mut child = spawn_bridge(&["--host", "link-host"]);
    let mut stdin = child.stdin.take().unwrap();
    let rx = read_head_then_tail(child.stdout.take().unwrap(), line.len());
    stdin.write_all(&line).unwrap();
    stdin.flush().unwrap();
    assert_eq!(next(&rx, "the link.hello, echoed by the relay"), line, "the shuttle answered link.hello itself");
    drop(stdin);
    let _ = next(&rx, "stdout after");
    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    assert_eq!(status.code(), Some(0), "{stderr}");
    echo.join().unwrap();
}

/// One request on a channel that carries the wire's frames, its reply read past any event.
#[cfg(target_os = "linux")]
async fn channel_call(
    tx: &mut sot_topology::link::ChannelWriter,
    rx: &mut tokio::io::BufReader<sot_topology::link::ChannelReader>,
    id: u64,
    op: &str,
    payload: serde_json::Value,
) -> sot_protocol::Frame {
    sot_protocol::codec::write_frame(tx, &sot_protocol::Frame::req(id, op, payload), None).await.expect("write a frame");
    loop {
        let (frame, _) = tokio::time::timeout(BOUND, sot_protocol::codec::read_frame(rx)).await.expect("a reply within the bound").expect("read a frame");
        if frame.id == id && frame.kind == sot_protocol::Kind::Res {
            return frame;
        }
    }
}

/// Topology 15: each channel reaches the daemon as a connection of its own, admitted by its own hello: two control
/// sessions and a handoff, all on one link, each answered as the daemon answers any connection.
#[cfg(target_os = "linux")]
#[test]
fn each_channel_is_admitted_as_its_own_connection() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = Env::new("bridge-channels");
    env.spawn_sotd();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        support::poll_until(|| async { support::try_connect(&env.socket_path).await }, BOUND, "sotd's socket").await;
    });
    let endpoint = format!("unix:{}", env.socket_path.display());
    let (_child, mut link) = bridge_link(&rt, &["--endpoint", &endpoint]);
    let handle = sot_topology::link::LinkHandle::default();
    handle.set_up(&link);
    rt.block_on(async {
        let hello = |client: &str, role: &str| {
            let req = sot_protocol::HelloReq::this_process(client, role, Some(TEST_STATE_HOST.to_string())).expect("this process's account");
            serde_json::to_value(req).unwrap()
        };
        let (r0, mut w0) = link.take_control().unwrap().into_split();
        let mut r0 = tokio::io::BufReader::new(r0);
        let first = channel_call(&mut w0, &mut r0, 1, sot_protocol::op::HELLO, hello("link-control", "cli")).await;
        assert!(first.payload.get("error").is_none(), "channel 0's hello: {:?}", first.payload);
        let (r1, mut w1) = handle.open().await.expect("open").into_split();
        let mut r1 = tokio::io::BufReader::new(r1);
        let second = channel_call(&mut w1, &mut r1, 1, sot_protocol::op::HELLO, hello("link-second", "cli")).await;
        assert!(second.payload.get("error").is_none(), "channel 1's hello: {:?}", second.payload);
        // A handoff channel: its hello and lane.connect, answered by the lane's own handler.
        let (r2, mut w2) = handle.open().await.expect("open").into_split();
        let mut r2 = tokio::io::BufReader::new(r2);
        let accepted = channel_call(&mut w2, &mut r2, 1, sot_protocol::op::HELLO, hello("link-lane", sot_protocol::HANDOFF_ROLE)).await;
        assert!(accepted.payload.get("error").is_none(), "the handoff's hello: {:?}", accepted.payload);
        let lane = channel_call(&mut w2, &mut r2, 2, sot_protocol::op::LANE_CONNECT, serde_json::json!({ "target": "no-such-row" })).await;
        assert_eq!(lane.payload["code"], "unknown_workspace", "the lane's own answer: {:?}", lane.payload);
        // A channel that says no hello is refused as any connection is.
        let (r3, mut w3) = handle.open().await.expect("open").into_split();
        let mut r3 = tokio::io::BufReader::new(r3);
        let refused = channel_call(&mut w3, &mut r3, 1, sot_protocol::op::VERSION_QUERY, serde_json::json!({})).await;
        assert_eq!(refused.payload["code"], "unauthenticated", "{:?}", refused.payload);
        let version = channel_call(&mut w0, &mut r0, 2, sot_protocol::op::VERSION_QUERY, serde_json::json!({})).await;
        let clients: Vec<&str> = version.payload["clients"].as_array().expect("clients").iter().filter_map(|c| c["client_id"].as_str()).collect();
        assert!(clients.contains(&"link-control") && clients.contains(&"link-second"), "two control sessions on one link: {clients:?}");
    });
    drop(link);
    rt.block_on(env.kill_daemon_bounded());
}

/// Topology 15: a channel's connection is `connect_own`'s, so a socket another OS account listens on at the endpoint's
/// path is refused at the channel's open, and that listener gets no byte. The endpoint is this account's when the link
/// starts; another account's listener is moved onto its path before the open. Linux: a zero byte count is evidence of
/// no write there only.
#[cfg(target_os = "linux")]
#[test]
fn a_channel_to_another_accounts_daemon_is_refused() {
    if !sot_platform::test_isolated::run_isolated("a_channel_to_another_accounts_daemon_is_refused") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(foreign) = sot_platform::test_foreign::ForeignListener::start(false) else {
        return;
    };
    let socket = runtime_root().join("link-foreign.sock");
    let _echo = EchoListener::bind(&socket);
    let endpoint = format!("unix:{}", socket.display());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (_child, link) = bridge_link(&rt, &["--endpoint", &endpoint]);
    let handle = sot_topology::link::LinkHandle::default();
    handle.set_up(&link);
    std::fs::rename(&foreign.path, &socket).expect("move the other account's socket onto the endpoint's path");
    let opened = rt.block_on(handle.open());
    match opened {
        Err(sot_topology::link::OpenError::Refused { code, detail }) => {
            assert_eq!(code, "unreachable", "{detail}");
            assert!(detail.contains("another OS account listens on this socket"), "{detail}");
        }
        other => panic!("a channel to another account's socket: {other:?}"),
    }
    // The socket goes back where its helper looks for it.
    std::fs::rename(&socket, &foreign.path).expect("move the socket back");
    drop(link);
    assert_eq!(foreign.finish(), 0, "the router sent another account's listener bytes");
}
