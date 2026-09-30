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
/// kills nor waits). Also holds the child's `stderr` (round-2 item 3): a
/// refused login used to reach the operator as the generic "no reply to
/// topology.set within 8 frames" -- ssh's own line ("Permission denied",
/// "Could not resolve hostname") is what `last_stderr_line` below folds
/// into the caller's error instead.
struct ChildGuard {
    child: std::process::Child,
    stderr: Option<std::process::ChildStderr>,
}

impl ChildGuard {
    /// Only ever called on an error path, after the caller has already
    /// decided to fail: kills the child (a no-op if it is already dead),
    /// then reads whatever is left on its stderr pipe to the end. Reading
    /// BEFORE that kill would block on a healthy, still-running child's
    /// open pipe -- the one thing this must never do to a connection that
    /// might otherwise still succeed.
    fn last_stderr_line(&mut self) -> Option<String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut stderr = self.stderr.take()?;
        use std::io::Read;
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf.lines().map(str::trim).rev().find(|l| !l.is_empty()).map(str::to_string)
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
                let stderr = child.stderr.take();
                Ok((Box::new(stdin), Box::new(stdout), Some(ChildGuard { child, stderr })))
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
    let (mut w, r, mut guard) = connect(endpoint)?.split().map_err(|e| format!("{endpoint}: {e}"))?;
    let mut br = std::io::BufReader::new(r);

    // Folds the ssh child's last non-empty stderr line into `msg` on any
    // error path below (round-2 item 3): a refused login otherwise
    // reaches the operator as the generic "no reply to topology.set
    // within 8 frames" -- the daemon looking mute when `ssh` was the
    // thing that failed. A no-op for unix:/tcp:/pipe: (`guard` is `None`
    // there, nothing to fold) and safe to call after every kind of
    // failure: `last_stderr_line` kills the child FIRST, so nothing here
    // can block on a healthy child's still-open pipe.
    let fold = |guard: &mut Option<ChildGuard>, msg: String| -> String {
        match guard.as_mut().and_then(ChildGuard::last_stderr_line) {
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
        .map_err(|e| fold(&mut guard, format!("{endpoint}: hello: {e}")))?;
    codec::read_frame_blocking(&mut br).map_err(|e| fold(&mut guard, format!("{endpoint}: hello reply: {e}")))?;

    const REQ_ID: u64 = 1;
    codec::write_frame_blocking(&mut w, &Frame::req(REQ_ID, req_op, payload))
        .map_err(|e| fold(&mut guard, format!("{endpoint}: {req_op}: {e}")))?;
    for _ in 0..8 {
        let frame = codec::read_frame_blocking(&mut br).map_err(|e| fold(&mut guard, format!("{endpoint}: {req_op} reply: {e}")))?;
        if frame.kind == Kind::Res && frame.id == REQ_ID {
            return Ok(frame.payload);
        }
    }
    Err(fold(&mut guard, format!("{endpoint}: no reply to {req_op} within 8 frames")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-2 item 4's `PATH` guard, restoring the exact original value
    /// on drop -- same shape as `julia.rs`'s own `EnvGuard`, copied
    /// rather than shared because that one is private to its module.
    struct EnvGuard(&'static str, Option<std::ffi::OsString>);
    impl EnvGuard {
        fn capture(key: &'static str) -> Self {
            Self(key, std::env::var_os(key))
        }
    }
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
        let script = dir.join("ssh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nsleep 5\n", marker.display())).expect("write stub ssh");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).expect("chmod stub ssh");
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
        let real_path = std::env::var_os("PATH").unwrap_or_default();
        let prepend = |dir: &std::path::Path| {
            std::env::set_var("PATH", std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&real_path))).expect("join PATH"));
        };

        let dir1 = tempfile::tempdir().expect("tempdir");
        let marker1 = dir1.path().join("invoked");
        write_stub_ssh(dir1.path(), &marker1);
        prepend(dir1.path());
        let conn = connect("ssh:hub").expect("ssh: must dispatch to the stub on PATH, not fail as unrecognised");
        let (_w, _r, guard) = conn.split().expect("split must hand back the stub's own stdin/stdout");
        assert!(guard.is_some(), "an ssh: connection must carry a ChildGuard, or dial_and_call returning early leaks the process");
        assert!(wait_for_marker(&marker1), "the ssh:hub arm must have exec'd OUR stub, not something else on PATH");
        drop(guard);

        let dir2 = tempfile::tempdir().expect("tempdir");
        let marker2 = dir2.path().join("invoked");
        write_stub_ssh(dir2.path(), &marker2);
        prepend(dir2.path());
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

    #[test]
    fn unrecognised_scheme_names_all_four_dialable_spellings() {
        let err = connect("carrier-pigeon:whatever").err().expect("must be an error");
        assert!(err.contains("unix:/tcp:/pipe:/ssh:"), "error should name all four schemes, got: {err}");
    }
}
