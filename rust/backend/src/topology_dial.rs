// topology_dial.rs — the one-shot blocking client `sotd topology set` (and
// `status`'s "cache diverged" line) use to reach a daemon over its
// already-established endpoint spelling (`unix:`/`tcp:`/`pipe:`/`ssh:`, per
// `topology::local_endpoint`/`relay_endpoint`). No new credential: per
// `op::TOPOLOGY_SET`'s own doc, the dial itself IS the authorisation, so
// this sends a plain unauthenticated `hello` (role `cli`) the same way any
// other one-shot shell caller would.
//
// Deliberately NOT the supervisor lane's `sot_log::client::Client` (pipe/
// socket challenge-auth trio): that machinery proves a CAPSULE's identity
// for the attach protocol, a different, heavier contract this plain
// control-socket hello has never needed.

use sot_protocol::{codec, Frame, HelloReq, Kind};

enum Conn {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
    #[cfg(windows)]
    Pipe(std::fs::File),
    /// An `ssh:` endpoint's connection IS the spawned child (C2/C3,
    /// `sot_protocol::ssh_bridge`) -- there is no separate "connect" step
    /// the way a socket has one, so this variant holds the not-yet-split
    /// `Child` rather than a stream.
    Bridged(std::process::Child),
}

/// Kills and reaps the ssh child `Conn::Bridged` hands to [`Conn::split`]
/// once its two halves are taken -- held by `dial_and_call` for as long as
/// the connection is open, so any early return (a bad hello, a reply that
/// never comes) cannot leak the process the way a bare `Child` dropped on
/// the floor would (the default `Drop` for `std::process::Child` neither
/// kills nor waits). Also carries the last line the child wrote to its
/// `stderr` (round-2 item 3): a refused login used to reach the operator
/// as the generic "no reply to topology.set within 8 frames" -- ssh's own
/// line ("Permission denied", "Could not resolve hostname") is what
/// `stderr_hint` below folds into the caller's error instead. That line arrives through the drain thread
/// `split()` spawns and is only ever READ here, never waited for: a
/// `read_to_string` on the child's stderr returns when EVERY write end of
/// that pipe is closed, and killing this child closes none of the ends its
/// own children inherited -- an operator's `ControlMaster`/`ControlPersist`
/// or `ProxyCommand` entry applies even though the argv here sets none of
/// them, and the mux master or proxy child then holds the pipe open. So the
/// error path used to hang where it now reports (round-3 blocker), and a
/// hang is strictly worse than a poor message.
struct ChildGuard {
    child: std::process::Child,
    last_stderr: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl ChildGuard {
    /// The child's own complaint, as far as its drain thread has read one:
    /// a clause that EXPLAINS an error the record could not, never a
    /// verdict of its own, and so never worth blocking for. A line the
    /// thread has not reached yet is simply absent.
    fn stderr_hint(&self) -> Option<String> {
        self.last_stderr.lock().ok().and_then(|line| line.clone())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Conn {
    /// Consume the connection and hand back its write half, its read
    /// half, and — for `Bridged` only — the guard that keeps the ssh
    /// child alive for as long as those halves are in use. A socket or
    /// pipe's two halves are the stream and its `try_clone()`, exactly
    /// what `try_clone()` + the old `Read`/`Write` impls gave
    /// `dial_and_call` before; `Bridged` takes the child's own stdin and
    /// stdout out of the `Child` instead of dialing anything.
    fn split(self) -> std::io::Result<(Box<dyn std::io::Write + Send>, Box<dyn std::io::Read + Send>, Option<ChildGuard>)> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => {
                let r = s.try_clone()?;
                Ok((Box::new(s), Box::new(r), None))
            }
            Conn::Tcp(s) => {
                let r = s.try_clone()?;
                Ok((Box::new(s), Box::new(r), None))
            }
            #[cfg(windows)]
            Conn::Pipe(f) => {
                let r = f.try_clone()?;
                Ok((Box::new(f), Box::new(r), None))
            }
            Conn::Bridged(mut child) => {
                let stdin = child.stdin.take().expect("spawn_sync pipes stdin");
                let stdout = child.stdout.take().expect("spawn_sync pipes stdout");
                // The child's last non-empty stderr line, drained on its own
                // thread for as long as the child lives -- the pattern
                // `rust/frontend/src/transport.rs` already uses for its own
                // ssh child (a task there, a thread here, since this path is
                // blocking). The thread outlives the call and is never joined:
                // it holds one pipe and ends at EOF, whereas waiting for that
                // EOF is precisely the hang `ChildGuard`'s doc describes.
                let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                if let Some(stderr) = child.stderr.take() {
                    let sink = std::sync::Arc::clone(&last_stderr);
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                            let line = line.trim();
                            if !line.is_empty() {
                                if let Ok(mut slot) = sink.lock() {
                                    *slot = Some(line.to_string());
                                }
                            }
                        }
                    });
                }
                Ok((Box::new(stdin), Box::new(stdout), Some(ChildGuard { child, last_stderr })))
            }
        }
    }
}

fn connect(endpoint: &str) -> Result<Conn, String> {
    if let Some(p) = endpoint.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            return std::os::unix::net::UnixStream::connect(p)
                .map(Conn::Unix)
                .map_err(|e| format!("{endpoint}: {e}"));
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            return Err(format!("{endpoint}: unix endpoints are POSIX-only"));
        }
    }
    if let Some(addr) = endpoint.strip_prefix("tcp:") {
        return std::net::TcpStream::connect(addr).map(Conn::Tcp).map_err(|e| format!("{endpoint}: {e}"));
    }
    if let Some(p) = endpoint.strip_prefix("pipe:") {
        #[cfg(windows)]
        {
            return std::fs::OpenOptions::new().read(true).write(true).open(p).map(Conn::Pipe).map_err(|e| format!("{endpoint}: {e}"));
        }
        #[cfg(not(windows))]
        {
            let _ = p;
            return Err(format!("{endpoint}: pipe endpoints are Windows-only"));
        }
    }
    if let Some(rest) = endpoint.strip_prefix("ssh:") {
        // Same two forms `rust/frontend/src/dial.rs` already parses --
        // `ssh:<target>` for that box's own daemon, `ssh:<target>/<host>`
        // for a daemon `<target>` relays to on `<host>`'s behalf — one
        // grammar, not a second one invented here.
        let (target, host) = match rest.split_once('/') {
            Some((t, h)) => (t, Some(h)),
            None => (rest, None),
        };
        let recipe = sot_protocol::ssh_bridge::SshRecipe::new(target, host).map_err(|e| format!("{endpoint}: {e}"))?;
        let child = sot_protocol::ssh_bridge::spawn_sync(&recipe).map_err(|e| format!("{endpoint}: {e}"))?;
        return Ok(Conn::Bridged(child));
    }
    Err(format!("{endpoint}: unrecognised endpoint spelling (expected unix:/tcp:/pipe:/ssh:)"))
}

/// Dial `endpoint`, send a `cli`-role hello declaring `self_host`, then one
/// `req_op` request, and return its response payload verbatim (the caller
/// checks for `{"error": ..., "code": ...}` itself — same convention every
/// other daemon refusal already uses). A handful of unrelated evt frames
/// arriving before the matching `res` (unlikely on a connection this
/// short-lived, but the wire protocol allows it) are skipped rather than
/// treated as a protocol violation.
pub fn dial_and_call(endpoint: &str, self_host: &str, req_op: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let (mut w, r, guard) = connect(endpoint)?.split().map_err(|e| format!("{endpoint}: {e}"))?;
    let mut br = std::io::BufReader::new(r);

    // Folds the ssh child's last non-empty stderr line into `msg` on any
    // error path below (round-2 item 3): a refused login otherwise
    // reaches the operator as the generic "no reply to topology.set
    // within 8 frames" -- the daemon looking mute when `ssh` was the
    // thing that failed. A no-op for unix:/tcp:/pipe: (`guard` is `None`
    // there, nothing to fold) and safe to call after every kind of
    // failure: it reads a value the drain thread parked, so no error path
    // can block on a pipe whose other writers this process does not
    // control (round-3 blocker). The child itself is killed and reaped by
    // `ChildGuard`'s `Drop`, on this path and every other.
    let fold = |guard: &Option<ChildGuard>, msg: String| -> String {
        match guard.as_ref().and_then(ChildGuard::stderr_hint) {
            Some(line) => format!("{msg}: {line}"),
            None => msg,
        }
    };

    let hello = HelloReq {
        client_id: format!("sotd-topology-cli-{}", std::process::id()),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some(self_host.to_string()),
        role: "cli".to_string(),
        instance: None,
        name: Some(self_host.to_string()),
    };
    let hello_payload = serde_json::to_value(hello).map_err(|e| e.to_string())?;
    codec::write_frame_blocking(&mut w, &Frame::req(0, sot_protocol::op::HELLO, hello_payload))
        .map_err(|e| fold(&guard, format!("{endpoint}: hello: {e}")))?;
    codec::read_frame_blocking(&mut br).map_err(|e| fold(&guard, format!("{endpoint}: hello reply: {e}")))?;

    const REQ_ID: u64 = 1;
    codec::write_frame_blocking(&mut w, &Frame::req(REQ_ID, req_op, payload))
        .map_err(|e| fold(&guard, format!("{endpoint}: {req_op}: {e}")))?;
    for _ in 0..8 {
        let frame = codec::read_frame_blocking(&mut br).map_err(|e| fold(&guard, format!("{endpoint}: {req_op} reply: {e}")))?;
        if frame.kind == Kind::Res && frame.id == REQ_ID {
            return Ok(frame.payload);
        }
    }
    Err(fold(&guard, format!("{endpoint}: no reply to {req_op} within 8 frames")))
}

/// A guest daemon's `comm.file` forward to its folder's hub (0031 B1): the
/// request as given, the hub's payload verbatim. [`dial_and_call`] sets no
/// deadline of its own, so the call runs on its own thread and no answer
/// within `within` is an error; a call that outlives the bound leaves its
/// thread to end when the connection does.
pub fn forward_comm_file(
    endpoint: &str,
    self_host: &str,
    req: &sot_protocol::CommFileReq,
    within: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    let (e, h) = (endpoint.to_string(), self_host.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(dial_and_call(&e, &h, sot_protocol::op::COMM_FILE, payload));
    });
    rx.recv_timeout(within).unwrap_or_else(|_| {
        Err(format!("{endpoint}: no reply to {} within {}s", sot_protocol::op::COMM_FILE, within.as_secs()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-2 item 4's `PATH` guard, restoring the exact original value
    /// on drop -- same shape as `julia.rs`'s own `EnvGuard`, copied
    /// rather than shared because that one is private to its module.
    #[cfg(unix)]
    struct EnvGuard(&'static str, Option<std::ffi::OsString>);
    #[cfg(unix)]
    impl EnvGuard {
        fn capture(key: &'static str) -> Self {
            Self(key, std::env::var_os(key))
        }
    }
    #[cfg(unix)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.1.take() {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

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
    fn write_ssh_script(dir: &std::path::Path, body: &str) {
        let script = dir.join("ssh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).expect("write stub ssh");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).expect("chmod stub ssh");
    }

    /// PREPENDS `dir` to `PATH` rather than replacing it -- see the first
    /// test below for why the rest of `PATH` has to survive.
    #[cfg(unix)]
    fn prepend_to_path(dir: &std::path::Path) {
        let real = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var("PATH", std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&real))).expect("join PATH"));
    }

    /// `Command::spawn()` (inside `connect`) returns as soon as the child
    /// process image exists, before the shell interpreter it execs has
    /// necessarily run far enough to touch `marker` -- so this polls
    /// rather than checking once, bounded so a genuinely wrong dispatch
    /// still fails promptly instead of hanging the suite.
    #[cfg(unix)]
    fn wait_for_marker(marker: &std::path::Path) -> bool {
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
    /// `ssh:` through `sot_protocol::ssh_bridge`, and `split()` must hand
    /// `dial_and_call` a `ChildGuard` for that arm so an early return
    /// cannot leak the spawned process -- proved here against a STUB
    /// `ssh` put FIRST on `PATH`, never the real one. The original
    /// version of this test called `connect("ssh:hub")` against whatever
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
        let conn = connect("ssh:hub").expect("ssh: must dispatch to the stub on PATH, not fail as unrecognised");
        let (_w, _r, guard) = conn.split().expect("split must hand back the stub's own stdin/stdout");
        assert!(guard.is_some(), "an ssh: connection must carry a ChildGuard, or dial_and_call returning early leaks the process");
        assert!(wait_for_marker(&marker1), "the ssh:hub arm must have exec'd OUR stub, not something else on PATH");
        drop(guard);

        let dir2 = tempfile::tempdir().expect("tempdir");
        let marker2 = dir2.path().join("invoked");
        write_stub_ssh(dir2.path(), &marker2);
        prepend_to_path(dir2.path());
        let conn = connect("ssh:hub/gamma").expect("ssh:<target>/<host> must parse the same way dial.rs does");
        let (_w, _r, guard) = conn.split().unwrap();
        assert!(wait_for_marker(&marker2), "the ssh:hub/gamma arm must have exec'd OUR stub too");
        drop(guard);

        // The same grammar SshRecipe::new enforces (frontend/src/dial.rs's
        // own parsing, C3) -- connect() delegates to it rather than
        // inventing a second check, so a value SshRecipe rejects must
        // fail HERE too, not just at the frontend. Neither call reaches
        // spawn_sync (the grammar check runs first), so PATH/the stub
        // above are irrelevant to these two.
        assert!(connect("ssh:-oProxyCommand=x").is_err(), "a leading-dash target must be refused");
        assert!(connect("ssh:Hub").is_err(), "an uppercase target must be refused (not a plain host name)");
    }

    /// Kills the stub's background `sleep` on every path out of the test.
    #[cfg(unix)]
    struct KillHolder(std::path::PathBuf);

    #[cfg(unix)]
    impl Drop for KillHolder {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.0) {
                let _ = std::process::Command::new("kill").arg(pid.trim()).status();
            }
        }
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
        let _holder = KillHolder(pid_file.clone());
        prepend_to_path(dir.path());

        let started = std::time::Instant::now();
        let err = dial_and_call("ssh:hub", "selfbox", "topology.set", serde_json::json!({}))
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
    }

    #[test]
    fn unrecognised_scheme_names_all_four_dialable_spellings() {
        let err = connect("carrier-pigeon:whatever").err().expect("must be an error");
        assert!(err.contains("unix:/tcp:/pipe:/ssh:"), "error should name all four schemes, got: {err}");
    }
}
