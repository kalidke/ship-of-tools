//! The listener: the daemon lock, the live-socket refusal and the local-socket accept loop.

use super::conn::handle_connection;
use super::*;
use sot_log::identity::challenge::PeerAuthenticated;
use sot_log::lane::accept_policy::{accept_policy, AcceptAction, ACCEPT_SLEEP};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
// Session-pipe hardening fix: on Windows `interprocess` leaves an unset
// security descriptor at its platform default (`Everyone`/`ANONYMOUS LOGON`
// read), so `bind_session` gives the pipe `session_pipe_security_descriptor`,
// built with these.
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::{
    AsSecurityDescriptorExt, BorrowedSecurityDescriptor,
};

/// `SOT_TEST_DAEMON_LOCK_WAIT_MS` overrides [`sot_protocol::ops::lease::DAEMON_LOCK_WAIT`]
/// for tests — the same `OnceLock` convention as [`ping_read_deadline`].
/// Unset in every real deployment.
fn daemon_lock_wait() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_DAEMON_LOCK_WAIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(sot_protocol::ops::lease::DAEMON_LOCK_WAIT)
}

/// Takes `<state_root>/daemon.lock`, trying every 250 ms. A busy lock with
/// a daemon answering on `socket` refuses at once; a busy lock with nobody
/// answering is a predecessor still shutting down, waited for up to
/// [`daemon_lock_wait`]. Either error ends `main` with exit 1.
pub(super) async fn lock_daemon(
    state_root: &std::path::Path,
    socket: Option<&std::path::Path>,
) -> Result<sot_log::host::DaemonLock> {
    let lock_path = sot_log::host::daemon_lock_path(state_root);
    let deadline = tokio::time::Instant::now() + daemon_lock_wait();
    let mut logged = false;
    loop {
        if let Some(lock) = sot_log::host::try_lock_daemon(state_root)
            .with_context(|| format!("open the daemon lock {}", lock_path.display()))?
        {
            return Ok(lock);
        }
        if let Some(path) = socket.filter(|p| socket_answers(p)) {
            anyhow::bail!(
                "another daemon on this computer holds {} and answers on {}; refusing to start",
                lock_path.display(),
                path.display()
            );
        }
        if !logged {
            tracing::info!(
                lock = %lock_path.display(),
                "waiting for the previous daemon on this computer to finish shutting down"
            );
            logged = true;
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the previous daemon on this computer still holds {} after {:?}",
                lock_path.display(),
                daemon_lock_wait()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Takes the daemon lock for `opts`, or runs unfenced when this machine has no state root.
pub(super) async fn take_daemon_lock(
    opts: &crate::Opts,
) -> Result<Option<sot_log::host::DaemonLock>> {
    Ok(match sot_log::host::state_dir::sot_state_dir() {
        Some(state_root) => Some(lock_daemon(&state_root, opts.socket.as_deref()).await?),
        None => {
            tracing::warn!(
                "daemon lock skipped, running unfenced: could not resolve this machine's state root \
                 ({} unset)",
                crate::rows::spawn::state_root::STATE_ROOT_HINT
            );
            None
        }
    })
}

/// Whether nothing listens on the socket at `path`, from what a probe's `connect_own` returned: the socket is missing
/// (`NotFound`) or its file is stale (`ConnectionRefused`, the kernel's answer when no socket listens on it). Anything
/// else, an answer from this account, another account's listener, a listener whose backlog is full (the connector
/// gave up at `CONNECT_BOUND`) or an error that cannot tell, is a listener or is not known to be none.
#[cfg(unix)]
fn nobody_listens(
    result: &Result<
        sot_log::lane::socket_unix::SocketClient,
        sot_log::lane::transport::TransportError,
    >,
) -> bool {
    use sot_log::lane::transport::TransportError;
    matches!(
        result,
        Err(TransportError::Io { op: "connect", source })
            if matches!(source.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
    )
}

/// Whether a daemon answers on the session socket: on Unix a socket something listens on (this account's, another
/// account's or one with a full backlog; `nobody_listens` is the rest), on Windows a client open of the pipe name
/// succeeds. The Unix probe is `connect_own`, bounded by `CONNECT_BOUND` and writing nothing.
fn socket_answers(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    return !nobody_listens(&sot_log::identity::connect_own::connect_own(path));
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Identification level, like every client of a pipe name another account may hold (ADR 0049, User isolation).
        return std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION)
            .open(path)
            .is_ok();
    }
}

/// Refuses when a daemon still answers on the socket at `path`. Unlinking
/// a live daemon's socket leaves it running on a deleted file that no
/// client can reach until it restarts. The probe is `connect_own`, bounded by `CONNECT_BOUND`, so another account's
/// listener and a listener with a full backlog refuse the start within that bound instead of hanging it. Only a
/// socket nobody answers on (ECONNREFUSED), or none at all, is stale (`nobody_listens`); any other result refuses too,
/// because it cannot tell.
#[cfg(unix)]
pub(crate) fn refuse_live_socket(path: &std::path::Path) -> Result<()> {
    let attempt = sot_log::identity::connect_own::connect_own(path);
    if nobody_listens(&attempt) {
        return Ok(());
    }
    match attempt {
        Ok(_) => anyhow::bail!(
            "another daemon is already listening on {}; refusing to start on its socket \
             (stop that daemon first, or pass a different --socket)",
            path.display()
        ),
        Err(e) => anyhow::bail!(
            "cannot tell whether a daemon is listening on {}: {e}; refusing to unlink its socket",
            path.display()
        ),
    }
}

/// The session pipe's security descriptor: protected, owner-only full
/// access, no `OI`/`CI` inheritance — built from `sot_log::
/// owner_protected_pipe_descriptor` (the SAME SDDL `lane/pipe_win/` already
/// uses for the voyage/supervisor pipes) rather than a second copy of that
/// string. `interprocess` has no ACL-building of its own to reuse, and
/// `PipeListenerOptions::security_descriptor` takes only its crate's own
/// `SecurityDescriptor` type, so this borrows the raw
/// descriptor `sot_log` built and clones it in (`to_owned_sd`) rather than
/// hand-rolling a second SDDL string here.
#[cfg(windows)]
fn session_pipe_security_descriptor(
) -> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    let descriptor = sot_log::owner_protected_pipe_descriptor()
        .context("build session pipe security descriptor")?;
    // SAFETY: `descriptor` was built by `ConvertStringSecurityDescriptorToSecurityDescriptorW`
    // (self-relative, valid, unaliased — see that function's own doc) and
    // outlives this call; `to_owned_sd` copies it into a fresh,
    // independently-owned `SecurityDescriptor` before `descriptor` (and its
    // `LocalFree` on drop) goes out of scope.
    let owned = unsafe { BorrowedSecurityDescriptor::from_ptr(descriptor.as_ptr()) }
        .to_owned_sd()
        .context("clone session pipe security descriptor")?;
    Ok(owned)
}

/// The session pipe's inbound buffer on Windows: a client's hello and its request, two envelopes of at most the cap
/// (`codec::MAX_ENVELOPE_BYTES`) each, so a client that writes both before it reads never blocks in its write
/// (`bind_session`). Its cost: a client of this account may leave up to this much nonpaged pool waiting per connection
/// until the daemon reads it or closes the pipe.
#[cfg(windows)]
const PIPE_INBOUND_BYTES: u32 = 2 * sot_protocol::codec::MAX_ENVELOPE_BYTES as u32;

/// The daemon's one listener, at `path` (ADR 0049 `## User isolation`). On Unix who may connect is decided by the
/// socket folder's mode, which `run_local` has made private. On Windows it is the pipe's owner-only descriptor, and the
/// pipe's inbound buffer holds a client's hello and its request, each one envelope at most the cap. After refusing a
/// hello the daemon reads no more, and interprocess holds the dropped pipe open until the client has read the refusal
/// (its limbo: `FlushFileBuffers` before the close), so a client that writes its hello and its request before it reads
/// (scripts/tests/pipe-request.ps1) must be able to finish writing without the daemon reading; with the default
/// 512-byte buffer a request longer than the daemon's read-ahead hung there until the client's own timeout. The local-socket builder
/// passes no buffer size, so the pipe is built with `PipeListenerOptions`, which it otherwise matches.
fn bind_session(path: &str) -> Result<interprocess::local_socket::tokio::Listener> {
    #[cfg(unix)]
    #[allow(
        clippy::disallowed_methods,
        reason = "listener: session socket or pipe: a private folder or an owner-only DACL"
    )]
    let listener = ListenerOptions::new()
        .name(
            path.to_fs_name::<GenericFilePath>()
                .with_context(|| format!("interpret {path:?} as local-socket name"))?,
        )
        .create_tokio()
        .with_context(|| format!("bind {path:?}"))?;
    #[cfg(windows)]
    #[allow(
        clippy::disallowed_methods,
        reason = "listener: session socket or pipe: a private folder or an owner-only DACL"
    )]
    let listener = interprocess::os::windows::named_pipe::PipeListenerOptions::new()
        .path(path)
        .security_descriptor(Some(session_pipe_security_descriptor()?))
        .input_buffer_size_hint(PIPE_INBOUND_BYTES)
        .create_tokio_duplex::<interprocess::os::windows::named_pipe::pipe_mode::Bytes>()
        .map(interprocess::os::windows::named_pipe::local_socket::tokio::Listener::from)
        .map(interprocess::local_socket::tokio::Listener::from)
        .with_context(|| format!("bind {path:?}"))?;
    Ok(listener)
}

pub(super) async fn run_local(
    socket_path: PathBuf,
    session: Session,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    preview_changed_tx: broadcast::Sender<PreviewChanged>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    links: crate::comm::mail::links::Links,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    topology_store: Arc<crate::topology::store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology::store::TopologyChanged>,
    leases: Arc<crate::lifecycle::lease::Leases>,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() {
            paths::secure_socket_dir(parent)
                .with_context(|| format!("secure socket dir {}", parent.display()))?;
        }
    }
    // Unix sockets leave a filesystem entry that blocks rebind; Windows
    // named pipes don't, so only do the cleanup on Unix, and only for a
    // socket nobody answers on.
    #[cfg(unix)]
    if std::path::Path::new(&socket_path).exists() {
        refuse_live_socket(std::path::Path::new(&socket_path))?;
        tokio::fs::remove_file(&socket_path)
            .await
            .with_context(|| format!("remove stale socket {socket_path:?}"))?;
    }

    let path_str = socket_path
        .to_str()
        .context("socket path must be valid UTF-8")?;
    let listener = bind_session(path_str)?;
    tracing::info!(socket = ?socket_path, "listening (local)");

    // The accept loop ends when a shutdown begins: shutdown step 1 stops
    // accepting by dropping the listener and unlinking the socket, on the
    // same wake as the deciding departure, before any row is touched.
    let decided = loop {
        #[allow(
            clippy::disallowed_methods,
            reason = "listener: session socket or pipe: a private folder or an owner-only DACL"
        )]
        let accept = listener.accept();
        let accepted = tokio::select! {
            accepted = accept => accepted,
            () = leases.gone() => break tokio::time::Instant::now(),
        };
        let stream: LocalStream = match accepted {
            Ok(stream) => stream,
            Err(e) => {
                survive_accept_error(e).await?;
                continue;
            }
        };
        dispatch_admitted(stream, admit_peer, |stream, peer| {
            let le = leases.clone();
            let s = session.clone();
            let mj = mathjax.clone();
            let pl = pluto.clone();
            let fm = files_mode.clone();
            let wa = preview_changed_tx.clone();
            let lb = label.clone();
            let ws = workspaces.clone();
            let wse = ws_events_tx.clone();
            let age = agent_events_tx.clone();
            let lk = links.clone();
            let fce = fe_command_tx.clone();
            let rfe = repl_frame_tx.clone();
            let cl = clients.clone();
            let tps = topology_store.clone();
            let tpe = topo_changed_tx.clone();
            tokio::spawn(async move {
                let (rx, tx) = stream.split();
                if let Err(e) = handle_connection(
                    rx, tx, s, mj, pl, fm, wa, lb, ws, wse, age, lk, fce, rfe, cl, tps, tpe, peer,
                    le,
                )
                .await
                {
                    tracing::warn!(error = %e, "connection ended with error");
                } else {
                    tracing::info!("connection closed");
                }
            });
        });
    };
    drop(listener);
    // The listener's drop already unlinks it; this covers a listener
    // that does not.
    #[cfg(unix)]
    match std::fs::remove_file(&socket_path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(socket = ?socket_path, error = %e, "shutdown: socket not unlinked");
        }
        _ => {}
    }
    crate::lifecycle::shutdown::run(leases, workspaces, ws_events_tx, decided).await
}

/// What the session loop does with an accept error, by the one accept policy (`sot_log::lane::accept_policy::accept_policy`): a
/// transient error returns at once, a resource error after a short sleep, and only a dead listener is an `Err`, which
/// ends the daemon.
async fn survive_accept_error(e: std::io::Error) -> Result<()> {
    match accept_policy(&e) {
        AcceptAction::Retry => Ok(()),
        AcceptAction::Sleep => {
            tracing::warn!(error = %e, "accept: out of descriptors or memory; waiting");
            tokio::time::sleep(ACCEPT_SLEEP).await;
            Ok(())
        }
        AcceptAction::Dead => Err(e).context("accept on sot socket"),
    }
}

/// One accepted-stream boundary: rejection drops the unread stream before its handler is entered; an admitted stream
/// goes to its handler with the peer's process, if the OS gave it (`admit_peer`).
fn dispatch_admitted(
    stream: LocalStream,
    observe: impl FnOnce(&LocalStream) -> Option<Admitted>,
    handle: impl FnOnce(LocalStream, Option<PeerAuthenticated>),
) -> bool {
    // Admission one: a connection whose account, as the OS recorded it at the connect, is another's, or that the
    // OS cannot read, is dropped before a byte is read.
    let Some(admitted) = observe(&stream) else {
        tracing::warn!(
            "refused a connection: the account the OS recorded for it is not this one, or could not be read"
        );
        return false;
    };
    handle(stream, admitted.process);
    true
}

/// A connection admission one let in: the peer's process if the OS gave it, else `None`, and the connection holds no
/// lease.
pub(crate) struct Admitted {
    pub(crate) process: Option<PeerAuthenticated>,
}

/// Whether the account behind a peer's effective uid is this process's own. A peer whose uid the OS did not give
/// is not ours.
#[cfg(unix)]
fn same_account(peer_euid: Option<u32>, own: u32) -> bool {
    peer_euid == Some(own)
}

/// The admission every connection passes first, at accept and before a byte is read (ADR 0049 `## User
/// isolation`): `None` when the connection's account is foreign or cannot be read, else `Admitted`, with the peer's
/// process (its pid and creation time) if the OS gives it to this daemon. Linux and macOS admit a connection whose recorded effective
/// uid is ours: `SO_PEERCRED` on Linux, and on macOS `challenge_macos::peer_euid_pid_created`, which reads it with
/// `getpeereid` and the pid and pidversion with `LOCAL_PEERTOKEN`. On Windows the pipe's owner-only descriptor has
/// already decided. Only a lease uses the process, and it is read apart from the account: on Windows its creation time
/// needs a handle to the peer's process, which a process of this account need not grant this daemon (an
/// administrator's ssh session grants only Administrators, SYSTEM and its own logon), and on Linux a peer that has
/// ended has none to read. Such a peer is served and holds no lease. On macOS the pid and creation time are live token
/// observations.
pub(crate) fn admit_peer(
    stream: &interprocess::local_socket::tokio::Stream,
) -> Option<Admitted> {
    admit_peer_observing(stream, |euid| euid)
}

fn admit_peer_observing(
    stream: &LocalStream,
    observed_account: impl FnOnce(Option<u32>) -> Option<u32>,
) -> Option<Admitted> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let interprocess::local_socket::tokio::Stream::UdSocket(s) = stream;
        let (euid, pid, created) =
            sot_log::identity::challenge_macos::peer_euid_pid_created(s.inner().as_raw_fd())
                .ok()?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        same_account(observed_account(Some(euid)), unsafe { libc::geteuid() })
            .then_some(Admitted { process: Some(PeerAuthenticated { pid, created }) })
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        use interprocess::local_socket::traits::StreamCommon as _;
        let creds = stream.peer_creds().ok();
        #[cfg(target_os = "linux")]
        {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let own = unsafe { libc::geteuid() };
            if !same_account(observed_account(creds.as_ref().and_then(|c| c.euid())), own) {
                return None;
            }
        }
        #[cfg(windows)]
        let _ = observed_account;
        let pid = creds.and_then(|c| c.pid()).and_then(|pid| u32::try_from(pid).ok());
        let process = pid.and_then(|pid| {
            let created = sot_log::identity::challenge::process_created(pid).ok()?;
            Some(PeerAuthenticated { pid, created })
        });
        Some(Admitted { process })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = stream;
        None
    }
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
