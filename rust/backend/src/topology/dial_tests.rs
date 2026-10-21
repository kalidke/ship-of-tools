use super::*;

/// ADR 0049, User isolation: `connect`'s `pipe:` arm opens the pipe at identification level, so a server that
/// impersonates the client right after the first byte gets an identification token.
#[cfg(windows)]
#[test]
fn the_pipe_arm_opens_at_identification_level() {
    use std::io::Write;
    let level = sot_log::identity::impersonation_probe::level_seen_by_server(|name| {
        let (mut writer, reader, guard) = connect(&format!("pipe:{name}"), crate::lifecycle::child_signal::process()).expect("connect").split().expect("split");
        writer.write_all(b"x").expect("write one byte");
        (writer, reader, guard)
    });
    assert_eq!(level, windows_sys::Win32::Security::SecurityIdentification);
}

#[cfg(unix)]
use crate::paths::EnvGuard;

/// Writes an executable `ssh` stub into `dir` that touches `marker`
/// then sleeps -- long enough for the test to see the marker land
/// and kill it via `ChildGuard`'s own `Drop`, never long enough to
/// outlive the test process if that kill were somehow skipped.
#[cfg(unix)]
fn write_stub_ssh(dir: &std::path::Path, marker: &std::path::Path) {
    write_ssh_script(dir, &format!("touch '{}'\nsleep 5\n", marker.display()));
}

/// Writes `body` as an executable `ssh` in `dir`, for a test that puts
/// `dir` first on `PATH`.
#[cfg(unix)]
pub(crate) fn write_ssh_script(dir: &std::path::Path, body: &str) {
    let script = dir.join("ssh");
    sot_log::test_exec::write_executable(&script, format!("#!/bin/sh\n{body}"));
}

/// PREPENDS `dir` to `PATH` rather than replacing it -- see the first
/// test below for why the rest of `PATH` has to survive.
#[cfg(unix)]
pub(crate) fn prepend_to_path(dir: &std::path::Path) {
    let real = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var("PATH", std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&real))).expect("join PATH"));
}

/// `Command::spawn()` (inside `connect`) returns as soon as the child
/// process image exists, before the shell interpreter it execs has
/// necessarily run far enough to touch `marker` -- so this polls
/// rather than checking once, bounded so a genuinely wrong dispatch
/// still fails promptly instead of hanging the suite.
#[cfg(unix)]
pub(crate) fn wait_for_marker(marker: &std::path::Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if marker.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    marker.exists()
}

/// BLOCKER 2, as amended by round-2 item 4: `connect` must dispatch
/// `ssh:` through `sot_protocol::topology::ssh_bridge`, and `split()` must hand
/// `dial_and_call` a `ChildGuard` for that arm so an early return
/// cannot leak the spawned process -- proved here against a STUB
/// `ssh` put FIRST on `PATH`, never the real one. The original
/// version of this test called `connect("ssh:hub", crate::lifecycle::child_signal::process())` against whatever
/// `ssh` the box actually had: `Command::spawn()` returns as soon as
/// the child process image exists, before it does any network
/// connecting, but the exec itself was still a real `ssh hub` attempt
/// -- a hard failure on any box with no `ssh` on `PATH`, and a
/// network reach a unit test has no business making. `dir` is
/// PREPENDED to the real `PATH`, not a replacement for it (the same
/// shape `comm-relay.sh`'s own test suite uses, `PATH="$sshdir:$PATH"`
/// in `relay_send_with_path`): the stub is what the bare name `ssh`
/// resolves to FIRST, but the stub script's own `touch`/`sleep` still
/// need the REST of `PATH` to run at all.
#[cfg(unix)]
#[test]
fn ssh_endpoint_dispatches_to_a_stub_on_path_never_the_network() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _path_guard = EnvGuard::capture("PATH");

    let dir1 = tempfile::tempdir().expect("tempdir");
    let marker1 = dir1.path().join("invoked");
    write_stub_ssh(dir1.path(), &marker1);
    prepend_to_path(dir1.path());
    let conn = connect("ssh:hub", crate::lifecycle::child_signal::process()).expect("ssh: must dispatch to the stub on PATH, not fail as unrecognised");
    let (_w, _r, guard) = conn.split().expect("split must hand back the stub's own stdin/stdout");
    assert!(guard.is_some(), "an ssh: connection must carry a ChildGuard, or dial_and_call returning early leaks the process");
    assert!(wait_for_marker(&marker1), "the ssh:hub arm must have exec'd OUR stub, not something else on PATH");
    drop(guard);

    let dir2 = tempfile::tempdir().expect("tempdir");
    let marker2 = dir2.path().join("invoked");
    write_stub_ssh(dir2.path(), &marker2);
    prepend_to_path(dir2.path());
    let conn = connect("ssh:hub/gamma", crate::lifecycle::child_signal::process()).expect("ssh:<target>/<host> must parse the same way dial.rs does");
    let (_w, _r, guard) = conn.split().unwrap();
    assert!(wait_for_marker(&marker2), "the ssh:hub/gamma arm must have exec'd OUR stub too");
    drop(guard);

    // The same grammar SshRecipe::new enforces (frontend/src/net/dial.rs's
    // own parsing, C3) -- connect() delegates to it rather than
    // inventing a second check, so a value SshRecipe rejects must
    // fail HERE too, not just at the frontend. Neither call reaches
    // spawn_sync (the grammar check runs first), so PATH/the stub
    // above are irrelevant to these two.
    assert!(connect("ssh:-oProxyCommand=x", crate::lifecycle::child_signal::process()).is_err(), "a leading-dash target must be refused");
    assert!(connect("ssh:Hub", crate::lifecycle::child_signal::process()).is_err(), "an uppercase target must be refused (not a plain host name)");
}

/// Dropping the dial takes the ssh child and what it started: an ssh
/// that starts a background process (a `ProxyCommand`) leaves nothing.
#[cfg(unix)]
#[test]
fn a_dropped_dial_takes_its_ssh_tree() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _path_guard = EnvGuard::capture("PATH");
    let dir = tempfile::tempdir().expect("tempdir");
    let bg = dir.path().join("bg");
    let descendant = crate::lifecycle::child_signal::tests::Leftover::of_file(bg.clone());
    write_ssh_script(dir.path(), &format!("sleep 3109 >/dev/null 2>&1 &\necho $! > '{}'\nexec cat\n", bg.display()));
    prepend_to_path(dir.path());
    let conn = connect("ssh:hub", crate::lifecycle::child_signal::process()).expect("connect");
    let (_w, _r, guard) = conn.split().expect("split");
    let began = std::time::Instant::now();
    while std::fs::read_to_string(&bg).map_or(true, |s| s.trim().is_empty()) {
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "the stub ssh never wrote its descendant's pid");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(guard);
    assert!(descendant.gone(), "the dial's ssh left a descendant alive");
}

/// ROUND-3 BLOCKER: no error path may WAIT on the ssh child's stderr.
/// The stub below leaves a background `sleep` holding that pipe after
/// exiting itself -- the shape a `ControlMaster` mux or a
/// `ProxyCommand` child has on a real box, and the shape
/// `write_stub_ssh` above already produces -- so a `read_to_string`
/// there returns only when the `sleep` does. Against the pre-fix
/// `ChildGuard::last_stderr_line` this call returned only when the
/// holder did, failing the assertion below; the
/// bound asserted is 10s against a holder that lives 30s, so the failure
/// mode is a failed assertion rather than a suite that hangs. The folded line
/// itself is deliberately NOT asserted: it is a hint the drain thread
/// may or may not have parked by the time the read half EOFs, and a
/// test of a race is worth less than the bound this one proves.
#[cfg(unix)]
#[test]
fn a_dead_child_whose_grandchild_holds_stderr_still_returns_promptly() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _path_guard = EnvGuard::capture("PATH");
    let dir = tempfile::tempdir().expect("tempdir");
    // Writes what a refused login writes, hands the inherited stderr to
    // a `sleep` that outlives it, and dies. Nothing answers the hello,
    // so `dial_and_call` takes an error path with a guard in hand.
    // The `sleep` keeps STDERR and drops stdout, which is what a
    // backgrounded `ControlPersist` master does (it redirects its own
    // stdout and keeps writing to the inherited stderr) -- and it is
    // also what makes this case specific: holding stdout too would
    // stall the reply read instead, a different wait with a different
    // cause, and the assertion below could no longer tell them apart.
    let pid_file = dir.path().join("holder.pid");
    write_ssh_script(
        dir.path(),
        &format!("echo 'Permission denied (publickey).' >&2\nsleep 30 >/dev/null &\necho $! > '{}'\nexit 255\n", pid_file.display()),
    );
    let holder = crate::lifecycle::child_signal::tests::Leftover::of_file(pid_file.clone());
    prepend_to_path(dir.path());

    let started = std::time::Instant::now();
    let err = dial_and_call("ssh:hub", "selfbox", "topology.set", serde_json::json!({}), std::time::Duration::from_secs(60))
        .expect_err("a child that answers nothing must be an error, not a hang");
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(10),
        "the error path waited {waited:?}: it must not read a pipe other processes still hold open (err: {err})"
    );
    // Promptly is only half of it: the verdict has to be the right one,
    // naming the endpoint and the step that failed rather than some
    // artefact of giving up on the pipe.
    assert!(
        err.contains("ssh:hub") && err.contains("hello"),
        "the error must name the endpoint and the step that failed, got: {err}"
    );
    assert!(holder.gone(), "the refused login's descendant outlived the error path");
}

/// A forwarded call is cut short by its own timeout or by the shutdown signal, and either way the ssh child dies
/// and is reaped.
#[cfg(unix)]
#[test]
fn a_dial_ends_at_its_timeout_or_the_shutdown_and_kills_its_ssh_child() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _path_guard = EnvGuard::capture("PATH");
    let payload = serde_json::json!({"from": "a", "to": "b", "text": "t"});
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) == 0 };
    for by_shutdown in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        write_ssh_script(dir.path(), &format!("echo $$ > '{}'\nexec sleep 30\n", pid_file.display()));
        prepend_to_path(dir.path());
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let within = std::time::Duration::from_secs(if by_shutdown { 20 } else { 1 });
        let firer = std::thread::spawn({
            let pid_file = pid_file.clone();
            move || {
                if by_shutdown {
                    assert!(wait_for_marker(&pid_file));
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    sig.fire().expect("fire");
                }
            }
        });
        let began = std::time::Instant::now();
        let result = dial_and_call_on(sig, "ssh:hub", "self", sot_protocol::op::COMM_FILE, payload.clone(), within);
        firer.join().unwrap();
        assert!(result.is_err());
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "the dial outlived its bound");
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let gone = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(30));
            !alive(pid)
        });
        assert!(gone, "the ssh child survived (shutdown={by_shutdown}) or was not reaped");
    }
}

/// A cancel kills the tracked child's tree through the guard's own handle on every platform, and the guard's drop
/// removes its registration. Fails on the unchanged Windows cancel, which only set the flag.
#[test]
fn cancel_kills_the_tracked_child_through_its_own_handle() {
    // Other tests swap PATH under this lock; the child below is found through PATH.
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let track = std::sync::Arc::new(Track::new(sig));
    #[cfg(unix)]
    let mut cmd = std::process::Command::new("sleep");
    #[cfg(unix)]
    cmd.arg("30");
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("ping");
    #[cfg(windows)]
    cmd.args(["-n", "31", "127.0.0.1"]);
    let child = sig
        .spawn_std(cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()))
        .expect("a sleeper child");
    #[cfg(unix)]
    assert_eq!(sig.held_groups().len(), 1);
    let mut guard = ChildGuard { child: std::sync::Arc::new(std::sync::Mutex::new(child)), last_stderr: Default::default(), track: None };
    assert!(!guard.attach(&track), "a fresh track is not cancelled");
    track.cancel();
    // A killed and reaped child is gone: on Unix its pid no longer answers `waitid`, on Windows its status is kept.
    let exited = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        guard.child.lock().unwrap().exited(false).unwrap_or(true)
    });
    assert!(exited, "cancel left the tracked child running");
    drop(guard);
    #[cfg(unix)]
    assert!(
        sig.held_groups().is_empty(),
        "the guard left a registered tree"
    );
    assert!(track.child.lock().unwrap().is_none(), "the guard left its child published after reaping it");
    track.cancel(); // after the guard is gone a cancel reaches nothing and must not panic
}

/// An ssh child's stderr may hold a byte that is not UTF-8 (a far host's banner): the drain thread reads on past
/// it, so the hint is still the child's last complaint.
#[cfg(unix)]
#[test]
fn the_ssh_hint_reads_past_a_byte_that_is_not_utf8() {
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", "printf 'Banner \\351t\\351\\nPermission denied (publickey).\\n' >&2"]);
    let child = sig
        .spawn_std(cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()))
        .expect("a stand-in ssh");
    let (_write, _read, guard) = Conn::Bridged(child).split().expect("split");
    let guard = guard.expect("a bridged child has a guard");
    let began = std::time::Instant::now();
    while std::sync::Arc::strong_count(&guard.last_stderr) > 1 {
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "the drain thread never ended");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(guard.stderr_hint().as_deref(), Some("Permission denied (publickey)."));
}

/// A daemon that refuses the hello closes the connection; the error is the refusal, not the next op's end of
/// file, and it carries the endpoint.
#[cfg(unix)]
#[test]
fn a_refused_hello_is_named() {
    use std::io::{BufRead, Write};
    const BOUND: std::time::Duration = std::time::Duration::from_secs(30);
    let dir = tempfile::Builder::new().prefix("sot-refuse-").permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700)).tempdir_in("/tmp").expect("tempdir");
    let path = dir.path().join("d.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let daemon = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut hello = String::new();
        std::io::BufReader::new(&conn).read_line(&mut hello).expect("read the hello");
        let refusal = Frame::res(0, sot_protocol::op::HELLO, serde_json::json!({ "error": "no thanks", "code": "os_user_conflict" }));
        let mut line = serde_json::to_vec(&refusal).unwrap();
        line.push(b'\n');
        conn.write_all(&line).expect("write the refusal");
    });
    let endpoint = format!("unix:{}", path.display());
    let err = dial_and_call(&endpoint, "host-a", "topology.set", serde_json::json!({}), std::time::Duration::from_secs(60))
        .expect_err("a refused hello is an error");
    sot_log::test_wait::within(BOUND, "the fake daemon", move || daemon.join()).unwrap();
    assert_eq!(err, format!("{endpoint}: hello refused: no thanks"));
}

#[test]
fn unrecognised_scheme_names_the_dialable_spellings() {
    let err = connect("carrier-pigeon:whatever", crate::lifecycle::child_signal::process()).err().expect("must be an error");
    assert!(err.contains("unix:/pipe:/ssh:"), "error should name all three schemes, got: {err}");
    let tcp = connect("tcp:127.0.0.1:1", crate::lifecycle::child_signal::process()).err().expect("a tcp endpoint is not dialable");
    assert!(tcp.contains("unrecognised endpoint spelling"), "{tcp}");
}

/// ADR 0049, User isolation: `connect`'s `unix:` arm refuses a socket another OS account listens on, and that
/// listener gets no byte.
#[cfg(unix)]
#[test]
fn a_socket_another_account_listens_on_is_refused() {
    if !sot_log::test_isolated::run_isolated(
        "topology::dial::tests::a_socket_another_account_listens_on_is_refused",
    ) {
        return;
    }
    let Some(foreign) = sot_log::test_foreign::ForeignListener::start(false) else {
        return;
    };
    let refused = match connect(
        &format!("unix:{}", foreign.path.display()),
        crate::lifecycle::child_signal::process(),
    ) {
        Ok(_) => String::from("connected"),
        Err(e) => e,
    };
    assert_eq!(
        foreign.finish(),
        0,
        "the dial sent another account's listener bytes"
    );
    assert!(
        refused.contains("another OS account listens on this socket"),
        "{refused}"
    );
}

/// ADR 0049, User isolation: `connect`'s `pipe:` arm refuses a pipe another account serves (`epmapper`, SYSTEM's).
#[cfg(windows)]
#[test]
fn a_pipe_another_account_serves_is_refused() {
    let refused = match connect(
        r"pipe:\\.\pipe\epmapper",
        crate::lifecycle::child_signal::process(),
    ) {
        Ok(_) => String::from("connected"),
        Err(e) => e,
    };
    assert!(refused.contains("not connecting"), "{refused}");
}

/// ADR 0049, User isolation: a dial to a socket whose backlog another account has filled returns within
/// `CONNECT_BOUND` plus 2 s of slack, and its error is not the account refusal, so the backlog was full.
#[cfg(unix)]
#[test]
fn a_full_foreign_backlog_ends_the_dial_within_its_bound() {
    if !sot_log::test_isolated::run_isolated(
        "topology::dial::tests::a_full_foreign_backlog_ends_the_dial_within_its_bound",
    ) {
        return;
    }
    let Some(foreign) = sot_log::test_foreign::ForeignListener::start(true) else {
        return;
    };
    let endpoint = format!("unix:{}", foreign.path.display());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ =
            tx.send(connect(&endpoint, crate::lifecycle::child_signal::process()).map(|_| ()));
    });
    let result = rx
        .recv_timeout(
            sot_log::lane::transport::CONNECT_BOUND + std::time::Duration::from_secs(2),
        )
        .expect("the dial did not end within its bound");
    let err = result.expect_err("connected through a full backlog");
    assert!(
        !err.contains("not connecting"),
        "the connect went through, so the backlog was not full: {err}"
    );
    assert_eq!(
        foreign.finish(),
        0,
        "the dial sent another account's listener bytes"
    );
}
