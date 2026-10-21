// topology/dial.rs — the one-shot blocking client `sotd topology set` (and
// `status`'s "cache diverged" line) use to reach a daemon over its
// already-established endpoint spelling (`unix:`/`pipe:`/`ssh:`, per
// `topology::endpoint::local_endpoint`/`relay_endpoint`). No new credential: per
// `op::TOPOLOGY_SET`'s own doc, the dial itself IS the authorisation, so
// this sends a plain unauthenticated `hello` (role `cli`) the same way any
// other one-shot shell caller would.
//
// Deliberately NOT the supervisor lane's `sot_log::lane::client::Client` (pipe/
// socket challenge-auth trio): that machinery proves a CAPSULE's identity
// for the attach protocol, a different, heavier contract this plain
// control-socket hello has never needed.

use sot_protocol::{codec, Frame, HelloReq, Kind};

enum Conn {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    Pipe(std::sync::Arc<sot_log::lane::pipe_win::PipeClient>),
    /// An `ssh:` endpoint's connection IS the spawned child (C2/C3,
    /// `sot_protocol::topology::ssh_bridge`) -- there is no separate "connect" step
    /// the way a socket has one, so this variant holds the not-yet-split
    /// [`ContainedStd`](crate::lifecycle::child_signal::ContainedStd) rather than a stream.
    Bridged(crate::lifecycle::child_signal::ContainedStd),
}

/// A blocking `Read`/`Write` view of a connected `PipeClient`, for `dial_and_call`'s `BufReader` and writer. Both halves
/// share one `Arc<PipeClient>`; `read` and `write_all` are blocking from the caller's view and reject a concurrent
/// same-direction submission, so the writer and the reply reader never share one slot.
#[cfg(windows)]
struct PipeIo(std::sync::Arc<sot_log::lane::pipe_win::PipeClient>);

#[cfg(windows)]
impl std::io::Read for PipeIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.0.read(buf).map_err(std::io::Error::other)
    }
}

#[cfg(windows)]
impl std::io::Write for PipeIo {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.0
            .write_all(buf)
            .map(|()| buf.len())
            .map_err(std::io::Error::other)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
/// own children inherited -- an operator's `ProxyCommand` entry applies,
/// and the proxy child then holds the pipe open. So the
/// error path used to hang where it now reports (round-3 blocker), and a
/// hang is strictly worse than a poor message.
struct ChildGuard {
    /// Shared with a [`Track`] so a cancel kills the child through this same handle.
    child: std::sync::Arc<std::sync::Mutex<crate::lifecycle::child_signal::ContainedStd>>,
    last_stderr: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Set for a forwarded call that a caller may cut short; see [`Track`].
    track: Option<std::sync::Arc<Track>>,
}

/// How a forwarding caller reaches the ssh child a dial thread owns: the
/// child itself while its guard holds it unreaped, and a flag that tells a
/// thread that has not spawned yet not to bother.
struct Track {
    sig: &'static crate::lifecycle::child_signal::Signal,
    /// The dial thread's child while its guard holds it unreaped; `None` before the spawn and from the moment the
    /// guard starts reaping.
    child: std::sync::Mutex<Option<std::sync::Arc<std::sync::Mutex<crate::lifecycle::child_signal::ContainedStd>>>>,
    cancelled: std::sync::atomic::AtomicBool,
}

impl Track {
    fn new(sig: &'static crate::lifecycle::child_signal::Signal) -> Self {
        Self { sig, child: std::sync::Mutex::new(None), cancelled: std::sync::atomic::AtomicBool::new(false) }
    }

    /// Kill the child's tree, if there is one yet; the dial thread's blocking
    /// read then fails and its `ChildGuard` drops.
    fn cancel(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.cancelled.store(true, SeqCst);
        // Held across the kill: the guard takes the child out of this slot before it drops, and the kill reaches the
        // tree through its own handle, never a pid another process may now hold.
        let slot = self.child.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(child) = slot.as_ref() {
            let _ = child.lock().unwrap_or_else(|p| p.into_inner()).kill();
        }
    }
}

impl ChildGuard {
    /// The child's own complaint, as far as its drain thread has read one:
    /// a clause that EXPLAINS an error the record could not, never a
    /// verdict of its own, and so never worth blocking for. A line the
    /// thread has not reached yet is simply absent.
    fn stderr_hint(&self) -> Option<String> {
        self.last_stderr.lock().ok().and_then(|line| line.clone())
    }

    /// Hand this guard's child to `track`; true when the track was cancelled before this.
    fn attach(&mut self, track: &std::sync::Arc<Track>) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.track = Some(std::sync::Arc::clone(track));
        *track.child.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::sync::Arc::clone(&self.child));
        track.cancelled.load(SeqCst)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Unpublish first, so no cancel can reach the child once it is reaped.
        if let Some(track) = &self.track {
            track.child.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
        // The tree is killed before the child is reaped: its pid is the group's number.
        let _ = self.child.lock().unwrap_or_else(|p| p.into_inner()).kill();
    }
}

impl Conn {
    /// Consume the connection and hand back its write half, its read
    /// half, and — for `Bridged` only — the guard that keeps the ssh
    /// child alive for as long as those halves are in use. A socket or
    /// pipe's two halves are the stream and its `try_clone()`, exactly
    /// what `try_clone()` + the old `Read`/`Write` impls gave
    /// `dial_and_call` before (a connected pipe's two halves are two `PipeIo`
    /// views of one client); `Bridged` takes the child's own stdin and
    /// stdout out of the `Child` instead of dialing anything.
    fn split(self) -> std::io::Result<(Box<dyn std::io::Write + Send>, Box<dyn std::io::Read + Send>, Option<ChildGuard>)> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => {
                let r = s.try_clone()?;
                Ok((Box::new(s), Box::new(r), None))
            }
            #[cfg(windows)]
            Conn::Pipe(client) => Ok((
                Box::new(PipeIo(std::sync::Arc::clone(&client))),
                Box::new(PipeIo(client)),
                None,
            )),
            Conn::Bridged(mut child) => {
                let stdin = child.stdin.take().expect("the ssh command pipes stdin");
                let stdout = child.stdout.take().expect("the ssh command pipes stdout");
                // The child's last non-empty stderr line, drained on its own
                // thread for as long as the child lives -- the pattern
                // `rust/frontend/src/net/transport/mod.rs` already uses for its own
                // ssh child (a task there, a thread here, since this path is
                // blocking). The thread outlives the call and is never joined:
                // it holds one pipe and ends at EOF, whereas waiting for that
                // EOF is precisely the hang `ChildGuard`'s doc describes.
                let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                if let Some(stderr) = child.stderr.take() {
                    let sink = std::sync::Arc::clone(&last_stderr);
                    std::thread::spawn(move || {
                        for line in sot_protocol::child_lines::blocking_child_lines(stderr).map_while(Result::ok) {
                            let line = line.trim();
                            if !line.is_empty() {
                                if let Ok(mut slot) = sink.lock() {
                                    *slot = Some(line.to_string());
                                }
                            }
                        }
                    });
                }
                Ok((Box::new(stdin), Box::new(stdout), Some(ChildGuard { child: std::sync::Arc::new(std::sync::Mutex::new(child)), last_stderr, track: None })))
            }
        }
    }
}

fn connect(endpoint: &str, sig: &'static crate::lifecycle::child_signal::Signal) -> Result<Conn, String> {
    if let Some(p) = endpoint.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            // ADR 0049, User isolation: `connect_own` checks the listener's cached account after its fixed-budget
            // connect, before client I/O.
            return sot_log::identity::connect_own::connect_own(std::path::Path::new(p))
                .map(|client| Conn::Unix(client.into_stream()))
                .map_err(|e| format!("{endpoint}: {e}"));
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            return Err(format!("{endpoint}: unix endpoints are POSIX-only"));
        }
    }
    if let Some(p) = endpoint.strip_prefix("pipe:") {
        #[cfg(windows)]
        {
            // ADR 0049, User isolation: `connect_own` opens the pipe at identification level and checks the serving
            // process's account before a byte is written.
            let client = sot_log::identity::connect_own::connect_own(std::path::Path::new(p))
                .map_err(|e| format!("{endpoint}: {e}"))?;
            return Ok(Conn::Pipe(std::sync::Arc::new(client)));
        }
        #[cfg(not(windows))]
        {
            let _ = p;
            return Err(format!("{endpoint}: pipe endpoints are Windows-only"));
        }
    }
    if let Some(rest) = endpoint.strip_prefix("ssh:") {
        // Same two forms `rust/frontend/src/net/dial.rs` already parses --
        // `ssh:<target>` for that box's own daemon, `ssh:<target>/<host>`
        // for a daemon `<target>` relays to on `<host>`'s behalf — one
        // grammar, not a second one invented here.
        let (target, host) = match rest.split_once('/') {
            Some((t, h)) => (t, Some(h)),
            None => (rest, None),
        };
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new(target, host).map_err(|e| format!("{endpoint}: {e}"))?;
        let mut cmd = sot_protocol::topology::ssh_bridge::LinkGate::default().command(&recipe).map_err(|e| format!("{endpoint}: {e}"))?;
        let child = sig.spawn_std(&mut cmd).map_err(|e| format!("{endpoint}: {e}"))?;
        return Ok(Conn::Bridged(child));
    }
    Err(format!("{endpoint}: unrecognised endpoint spelling (expected unix:/pipe:/ssh:)"))
}

/// Dial `endpoint`, send a `cli`-role hello declaring `self_host`, then one
/// `req_op` request, and return its response payload verbatim (the caller
/// checks for `{"error": ..., "code": ...}` itself — same convention every
/// other daemon refusal already uses). A handful of unrelated evt frames
/// arriving before the matching `res` (unlikely on a connection this
/// short-lived, but the wire protocol allows it) are skipped rather than
/// treated as a protocol violation.
///
/// The call owns its deadline: it runs on a thread of its own and waits at most `within`, or until the daemon's
/// shutdown signal, and at either it cancels its [`Track`] (a bridged ssh child's tree is killed) and returns an error
/// naming the endpoint and the op. A local socket's or pipe's blocked read is left to its thread, which ends with the
/// connection.
pub fn dial_and_call(
    endpoint: &str,
    self_host: &str,
    req_op: &str,
    payload: serde_json::Value,
    within: std::time::Duration,
) -> Result<serde_json::Value, String> {
    dial_and_call_on(crate::lifecycle::child_signal::process(), endpoint, self_host, req_op, payload, within)
}

/// [`dial_and_call`] under `sig`, the signal that starts the ssh child and ends the wait.
fn dial_and_call_on(
    sig: &'static crate::lifecycle::child_signal::Signal,
    endpoint: &str,
    self_host: &str,
    req_op: &str,
    payload: serde_json::Value,
    within: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (e, h, op) = (endpoint.to_string(), self_host.to_string(), req_op.to_string());
    let track = std::sync::Arc::new(Track::new(sig));
    let dial_track = std::sync::Arc::clone(&track);
    std::thread::spawn(move || {
        let _ = tx.send(dial_and_call_tracked(&e, &h, &op, payload, Some(dial_track)));
    });
    let deadline = std::time::Instant::now() + within;
    loop {
        let step = deadline.saturating_duration_since(std::time::Instant::now()).min(std::time::Duration::from_millis(50));
        match rx.recv_timeout(step) {
            Ok(done) => return done,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(format!("{endpoint}: the dial thread ended")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if sig.is_fired() {
            track.cancel();
            return Err(format!("{endpoint}: the daemon is shutting down"));
        }
        if std::time::Instant::now() >= deadline {
            track.cancel();
            return Err(format!("{endpoint}: no reply to {req_op} within {}s", within.as_secs()));
        }
    }
}

fn dial_and_call_tracked(
    endpoint: &str,
    self_host: &str,
    req_op: &str,
    payload: serde_json::Value,
    track: Option<std::sync::Arc<Track>>,
) -> Result<serde_json::Value, String> {
    let sig = track.as_ref().map_or_else(crate::lifecycle::child_signal::process, |t| t.sig);
    let (mut w, r, mut guard) = connect(endpoint, sig)?.split().map_err(|e| format!("{endpoint}: {e}"))?;
    if let (Some(track), Some(g)) = (&track, guard.as_mut()) {
        if g.attach(track) {
            return Err(format!("{endpoint}: cancelled"));
        }
    }
    let mut br = std::io::BufReader::new(r);

    // Folds the ssh child's last non-empty stderr line into `msg` on any
    // error path below (round-2 item 3): a refused login otherwise
    // reaches the operator as the generic "no reply to topology.set
    // within 8 frames" -- the daemon looking mute when `ssh` was the
    // thing that failed. A no-op for unix:/pipe: (`guard` is `None`
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

    let hello_payload = cli_hello(self_host).map_err(|e| fold(&guard, format!("{endpoint}: {e}")))?;
    codec::write_frame_blocking(&mut w, &Frame::req(0, sot_protocol::op::HELLO, hello_payload))
        .map_err(|e| fold(&guard, format!("{endpoint}: hello: {e}")))?;
    let reply = codec::read_frame_blocking(&mut br).map_err(|e| fold(&guard, format!("{endpoint}: hello reply: {e}")))?;
    // A refused hello closes the connection (ADR 0049 `## User isolation`); say why instead of failing the next op on
    // the closed link.
    if let Some(error) = reply.payload.get("error") {
        let error = error.as_str().map_or_else(|| error.to_string(), str::to_string);
        return Err(fold(&guard, format!("{endpoint}: hello refused: {error}")));
    }

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

/// The `cli` hello every call here sends: this process's identity, declaring `self_host` as its host and its name.
fn cli_hello(self_host: &str) -> Result<serde_json::Value, String> {
    let hello = HelloReq {
        name: Some(self_host.to_string()),
        ..HelloReq::this_process(format!("sotd-topology-cli-{}", std::process::id()), "cli", Some(self_host.to_string()))
            .map_err(|e| e.to_string())?
    };
    serde_json::to_value(hello).map_err(|e| e.to_string())
}

/// Dial `endpoint`, send the `cli` hello, and return the reply's payload verbatim, a refusal included: `sotd
/// same-version` reads the daemon's version from either. Waits at most `within`; a read still blocked then is left to
/// its thread, which ends with the process.
pub fn hello_reply(endpoint: &str, self_host: &str, within: std::time::Duration) -> Result<serde_json::Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (e, h) = (endpoint.to_string(), self_host.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(hello_reply_blocking(&e, &h));
    });
    rx.recv_timeout(within).map_err(|_| format!("{endpoint}: no hello reply within {}s", within.as_secs()))?
}

fn hello_reply_blocking(endpoint: &str, self_host: &str) -> Result<serde_json::Value, String> {
    let sig = crate::lifecycle::child_signal::process();
    let (mut w, r, _guard) = connect(endpoint, sig)?.split().map_err(|e| format!("{endpoint}: {e}"))?;
    let hello = cli_hello(self_host).map_err(|e| format!("{endpoint}: {e}"))?;
    codec::write_frame_blocking(&mut w, &Frame::req(0, sot_protocol::op::HELLO, hello))
        .map_err(|e| format!("{endpoint}: hello: {e}"))?;
    let reply = codec::read_frame_blocking(&mut std::io::BufReader::new(r))
        .map_err(|e| format!("{endpoint}: hello reply: {e}"))?;
    Ok(reply.payload)
}

#[cfg(test)]
#[path = "dial_tests.rs"]
pub(crate) mod tests;

#[cfg(all(test, unix))]
#[path = "dial_deadline_tests.rs"]
mod deadline_tests;
