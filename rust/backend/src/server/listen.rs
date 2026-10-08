//! The listener: the daemon lock, the live-socket refusal and the local-socket accept loop.

use super::conn::handle_connection;
use super::*;
use sot_log::identity::challenge::PeerAuthenticated;

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

/// Whether a daemon answers on the session socket: on Unix a connect
/// succeeds, on Windows a client open of the pipe name succeeds.
fn socket_answers(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    #[allow(
        clippy::disallowed_methods,
        reason = "the daemon probes the path it is about to bind; an answer refuses the start and nothing is written"
    )]
    return std::os::unix::net::UnixStream::connect(path).is_ok();
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
/// client can reach until it restarts. Only a socket nobody answers on
/// (ECONNREFUSED), or none at all, is stale; any other connect error
/// refuses too, because it cannot tell.
#[cfg(unix)]
pub(crate) fn refuse_live_socket(path: &std::path::Path) -> Result<()> {
    #[allow(
        clippy::disallowed_methods,
        reason = "the daemon probes the path it is about to bind; an answer refuses the start and nothing is written"
    )]
    let attempt = std::os::unix::net::UnixStream::connect(path);
    match attempt {
        Ok(_) => anyhow::bail!(
            "another daemon is already listening on {}; refusing to start on its socket \
             (stop that daemon first, or pass a different --socket)",
            path.display()
        ),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
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
    agent_receipt_tx: broadcast::Sender<AgentReceipt>,
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
        let stream: LocalStream = tokio::select! {
            accepted = accept => accepted.context("accept on sot socket")?,
            () = leases.gone() => break tokio::time::Instant::now(),
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
            let agr = agent_receipt_tx.clone();
            let fce = fe_command_tx.clone();
            let rfe = repl_frame_tx.clone();
            let cl = clients.clone();
            let tps = topology_store.clone();
            let tpe = topo_changed_tx.clone();
            tokio::spawn(async move {
                let (rx, tx) = stream.split();
                if let Err(e) = handle_connection(
                    rx, tx, s, mj, pl, fm, wa, lb, ws, wse, age, agr, fce, rfe, cl, tps, tpe, peer,
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

/// One accepted-stream boundary: rejection drops the unread stream before its handler is entered.
fn dispatch_admitted(
    stream: LocalStream,
    observe: impl FnOnce(&LocalStream) -> Option<PeerAuthenticated>,
    handle: impl FnOnce(LocalStream, PeerAuthenticated),
) -> bool {
    // Admission one: a connection whose account, as the OS recorded it at the connect, is another's, or that the
    // OS cannot read, is dropped before a byte is read.
    let Some(peer) = observe(&stream) else {
        tracing::warn!(
            "refused a connection: the account the OS recorded for it is not this one, or could not be read"
        );
        return false;
    };
    handle(stream, peer);
    true
}

/// Whether the account behind a peer's effective uid is this process's own. A peer whose uid the OS did not give
/// is not ours.
#[cfg(unix)]
fn same_account(peer_euid: Option<u32>, own: u32) -> bool {
    peer_euid == Some(own)
}

/// The admission every connection passes first, at accept and before a byte is read (ADR 0049 `## User
/// isolation`): cached connection provenance plus a pid/creation observation, or `None` when the account is foreign
/// or the read fails. Linux and macOS admit a connection whose recorded effective uid is ours: `SO_PEERCRED` on Linux,
/// and on macOS `challenge_macos::peer_euid_pid_created`, which reads it with `getpeereid` and the pid and pidversion
/// with `LOCAL_PEERTOKEN`. On Windows the pipe's owner-only descriptor has already decided, and the pid is read here.
/// The pid and creation time ride on for a lease; on macOS these are live token observations.
pub(crate) fn admit_peer(
    stream: &interprocess::local_socket::tokio::Stream,
) -> Option<PeerAuthenticated> {
    admit_peer_observing(stream, |euid| euid)
}

fn admit_peer_observing(
    stream: &LocalStream,
    observed_account: impl FnOnce(Option<u32>) -> Option<u32>,
) -> Option<PeerAuthenticated> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let interprocess::local_socket::tokio::Stream::UdSocket(s) = stream;
        let (euid, pid, created) =
            sot_log::identity::challenge_macos::peer_euid_pid_created(s.inner().as_raw_fd())
                .ok()?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        same_account(observed_account(Some(euid)), unsafe { libc::geteuid() })
            .then_some(PeerAuthenticated { pid, created })
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        use interprocess::local_socket::traits::StreamCommon as _;
        let creds = stream.peer_creds().ok()?;
        #[cfg(target_os = "linux")]
        {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let own = unsafe { libc::geteuid() };
            if !same_account(observed_account(creds.euid()), own) {
                return None;
            }
        }
        #[cfg(windows)]
        let _ = observed_account;
        let pid = u32::try_from(creds.pid()?).ok()?;
        let created = sot_log::identity::challenge::process_created(pid).ok()?;
        Some(PeerAuthenticated { pid, created })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = stream;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use interprocess::local_socket::{GenericFilePath, ListenerOptions};

    /// ADR 0049 `## User isolation`: a Unix connection is admitted only when its kernel-recorded effective uid is this
    /// account's; foreign or missing records are refused. On Linux and macOS this is the account recorded at the connect,
    /// not the holder's current euid; on macOS the pid is the live token's observation.
    #[cfg(unix)]
    #[test]
    fn same_account_table() {
        for (peer, own, admitted) in [
            (Some(1000), 1000, true),
            (Some(0), 0, true),
            (Some(1001), 1000, false),
            (Some(0), 1000, false),
            (Some(1000), 0, false),
            (None, 1000, false),
            (None, 0, false),
        ] {
            assert_eq!(
                same_account(peer, own),
                admitted,
                "peer {peer:?}, own {own}"
            );
        }
    }

    /// This process, dialling its own listener, is admitted with its own pid and creation time (the macOS
    /// `pidversion`, read from `LOCAL_PEERTOKEN`).
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[tokio::test]
    async fn admit_peer_admits_this_process_with_its_pid_and_creation_time() {
        #[cfg(unix)]
        let (path, _dir) = {
            let dir = tempfile::Builder::new()
                .prefix("sot-admit-")
                .tempdir_in("/tmp")
                .expect("socket folder");
            (dir.path().join("a.sock"), dir)
        };
        #[cfg(windows)]
        let (path, _dir) = (
            std::path::PathBuf::from(format!(r"\\.\pipe\sot-admit-test-{}", std::process::id())),
            (),
        );
        let name = || {
            path.to_str()
                .unwrap()
                .to_fs_name::<GenericFilePath>()
                .unwrap()
        };
        let listener = ListenerOptions::new()
            .name(name())
            .create_tokio()
            .expect("listen");
        let (accepted, dialled) = tokio::join!(listener.accept(), LocalStream::connect(name()));
        let (stream, _client) = (accepted.expect("accept"), dialled.expect("dial"));
        let peer = admit_peer(&stream).expect("this process is admitted");
        assert_eq!(peer.pid, std::process::id());
        #[cfg(target_os = "macos")]
        let created = u64::from(
            sot_log::identity::challenge_macos::self_pidversion().expect("own pidversion"),
        );
        #[cfg(not(target_os = "macos"))]
        let created = sot_log::identity::challenge::process_created(std::process::id())
            .expect("own creation time");
        assert_eq!(peer.created, created);
    }

    /// ADR 0049, User isolation: real peer lookup precedes the controlled account result and dispatch.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_os_peer_reaches_no_connection_handler() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for refuse in [Some(true), None, Some(false)] {
            let root = tempfile::Builder::new()
                .prefix("sot-boundary-")
                .tempdir_in("/tmp")
                .unwrap();
            let path = root.path().join("s.sock");
            let listener = bind_session(path.to_str().unwrap()).unwrap();
            let name = path.to_fs_name::<GenericFilePath>().unwrap();
            let (accepted, client) = tokio::join!(listener.accept(), LocalStream::connect(name));
            let mut client = client.unwrap();
            let observed = AtomicUsize::new(0);
            let handlers = AtomicUsize::new(0);
            let mut admitted = None;
            let dispatched = dispatch_admitted(
                accepted.unwrap(),
                |stream| {
                    assert!(
                        admit_peer(stream).is_some(),
                        "native peer-reader prerequisite failed"
                    );
                    admit_peer_observing(stream, |euid| {
                        observed.fetch_add(1, SeqCst);
                        assert!(euid.is_some(), "kernel did not report credentials");
                        match refuse {
                            Some(true) => euid.map(|uid| uid.wrapping_add(1)),
                            None => None,
                            Some(false) => euid,
                        }
                    })
                },
                |stream, _| {
                    handlers.fetch_add(1, SeqCst);
                    admitted = Some(stream);
                },
            );
            assert_eq!(observed.load(SeqCst), 1);
            if refuse == Some(false) {
                assert!(dispatched);
                let mut stream = admitted.unwrap();
                stream.write_all(b"served").await.unwrap();
                let mut marker = [0; 6];
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    client.read_exact(&mut marker),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(&marker, b"served");
                assert_eq!(handlers.load(SeqCst), 1);
            } else {
                assert_eq!(
                    handlers.load(SeqCst),
                    0,
                    "refused OS peer reached a connection handler"
                );
                let mut bytes = Vec::new();
                let ended = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    client.read_to_end(&mut bytes),
                )
                .await;
                assert!(ended.is_ok(), "refusal did not close");
                assert!(
                    !dispatched && bytes.is_empty(),
                    "rejected peer received a reply"
                );
                assert_eq!(
                    handlers.load(SeqCst),
                    0,
                    "refused OS peer reached a connection handler"
                );
                eprintln!("admission-proof test=server::listen::tests::a_refused_os_peer_reaches_no_connection_handler endpoint=session boundary=owner-query fixture=controlled-owner-result rejected=true dispatched=0 bodies=1");
            }
        }
    }

    /// Twin of `sot-log`'s own
    /// `pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags`
    /// (`rust/log/tests/pipe_win/`) — same technique (`GetSecurityInfo` on
    /// a LIVE handle, round-tripped to SDDL) — but against THIS crate's
    /// session pipe rather than a voyage/supervisor pipe: proves
    /// `bind_session`, the listener `run_local` binds, wires
    /// `session_pipe_security_descriptor()` into the pipe, not merely that
    /// the descriptor builds correct bytes in isolation. Before this fix the
    /// session pipe carried the Windows default (`Everyone`/`ANONYMOUS LOGON`
    /// read).
    #[cfg(windows)]
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one test scenario: the session pipe's security descriptor, checked flag by flag"
    )]
    async fn session_pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
        use sot_log::host::wide_null;
        use windows_sys::Win32::Foundation::{
            CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, READ_CONTROL,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        fn current_user_sid_string() -> String {
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                assert_ne!(
                    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
                    0
                );
                let mut needed: u32 = 0;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
                assert!(
                    needed > 0,
                    "GetTokenInformation sizing call returned zero length"
                );
                let words = (needed as usize).div_ceil(8);
                let mut buf: Vec<u64> = vec![0u64; words];
                let buf_ptr = buf.as_mut_ptr().cast::<u8>();
                assert_ne!(
                    GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed),
                    0
                );
                let sid = (*buf_ptr.cast::<TOKEN_USER>()).User.Sid;
                let mut sid_str: *mut u16 = std::ptr::null_mut();
                assert_ne!(ConvertSidToStringSidW(sid, &mut sid_str), 0);
                let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
                LocalFree(sid_str as _);
                CloseHandle(token);
                s
            }
        }

        /// Round-trip a LIVE PIPE HANDLE's DACL to SDDL text via
        /// `GetSecurityInfo` (Microsoft directs named-pipe security queries
        /// through the HANDLE-based `GetSecurityInfo`, not the name-based
        /// `GetNamedSecurityInfoW`).
        fn security_descriptor_sddl(handle: HANDLE) -> String {
            unsafe {
                let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
                let rc = GetSecurityInfo(
                    handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut psd,
                );
                assert_eq!(rc, 0, "GetSecurityInfo failed: {rc}");
                let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
                let mut sddl_len: u32 = 0;
                let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    psd,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut sddl_ptr,
                    &mut sddl_len,
                );
                assert_ne!(
                    ok, 0,
                    "ConvertSecurityDescriptorToStringSecurityDescriptorW failed"
                );
                let len = (0..).take_while(|&i| *sddl_ptr.add(i) != 0).count();
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(sddl_ptr, len));
                LocalFree(sddl_ptr as _);
                LocalFree(psd as _);
                s
            }
        }

        /// Round-trip an SDDL STRING through the converter pair to ITS
        /// canonical form, so the expected side speaks the same
        /// well-known-SID-aliasing dialect the actual side comes back in.
        fn canonical_sddl(sddl: &str) -> String {
            let wide_sddl = wide_null(sddl);
            unsafe {
                let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
                assert_ne!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        wide_sddl.as_ptr(),
                        SDDL_REVISION_1,
                        &mut psd,
                        std::ptr::null_mut(),
                    ),
                    0,
                    "string->SD failed for {sddl}"
                );
                let mut out_ptr: *mut u16 = std::ptr::null_mut();
                let mut out_len: u32 = 0;
                let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    psd,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut out_ptr,
                    &mut out_len,
                );
                assert_ne!(ok, 0, "SD->string failed for {sddl}");
                let len = (0..).take_while(|&i| *out_ptr.add(i) != 0).count();
                let out = String::from_utf16_lossy(std::slice::from_raw_parts(out_ptr, len));
                LocalFree(out_ptr as _);
                LocalFree(psd as _);
                out
            }
        }

        let name = format!(r"\\.\pipe\sot-test-session-acl-{}", std::process::id());
        let listener = super::bind_session(&name).unwrap();

        let wide_name = wide_null(&name);
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            handle,
            INVALID_HANDLE_VALUE,
            "CreateFileW failed: {:?}",
            std::io::Error::last_os_error()
        );

        let sid = current_user_sid_string();
        let expected = canonical_sddl(&format!("D:P(A;;FA;;;{sid})"));
        let actual = security_descriptor_sddl(handle);
        unsafe { CloseHandle(handle) };

        assert_eq!(actual, expected);
        assert!(
            !actual.contains("OICI"),
            "session pipe descriptor must carry no OI/CI flags: {actual}"
        );

        drop(listener);
    }
    #[cfg(windows)]
    /// Native access check on a synchronous test-owned client thread; impersonation never crosses an await.
    fn restricted_open_denied(path: String) -> std::io::Error {
        std::thread::spawn(move || {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            use windows_sys::Win32::Security::{
                CreateRestrictedToken, CreateWellKnownSid, ImpersonateLoggedOnUser, RevertToSelf,
                WinWorldSid, SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY,
            };
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
            struct Token(HANDLE);
            impl Drop for Token {
                fn drop(&mut self) {
                    unsafe {
                        CloseHandle(self.0);
                    }
                }
            }
            struct Restore;
            impl Drop for Restore {
                fn drop(&mut self) {
                    assert_ne!(
                        unsafe { RevertToSelf() },
                        0,
                        "restore test-owned client token"
                    );
                }
            }
            let mut original = Token(std::ptr::null_mut());
            assert_ne!(
                unsafe {
                    OpenProcessToken(
                        GetCurrentProcess(),
                        TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_QUERY,
                        &mut original.0,
                    )
                },
                0,
                "open test-owned token"
            );
            let mut sid = [0u32; 17];
            let mut bytes = std::mem::size_of_val(&sid) as u32;
            assert_ne!(
                unsafe {
                    CreateWellKnownSid(
                        WinWorldSid,
                        std::ptr::null_mut(),
                        sid.as_mut_ptr().cast(),
                        &mut bytes,
                    )
                },
                0,
                "create restricted access SID"
            );
            let restriction = SID_AND_ATTRIBUTES {
                Sid: sid.as_mut_ptr().cast(),
                Attributes: 0,
            };
            let mut restricted = Token(std::ptr::null_mut());
            assert_ne!(
                unsafe {
                    CreateRestrictedToken(
                        original.0,
                        0,
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        1,
                        &restriction,
                        &mut restricted.0,
                    )
                },
                0,
                "create restricted test token"
            );
            assert_ne!(
                unsafe { ImpersonateLoggedOnUser(restricted.0) },
                0,
                "impersonate restricted test token"
            );
            let _restore = Restore;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                {
                    Err(error)
                        if error.raw_os_error() == Some(231)
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    result => {
                        break result.expect_err("pipe opened for a token without the owner grant")
                    }
                }
            }
        })
        .join()
        .expect("test-owned access thread panicked")
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn session_pipe_denies_a_token_without_the_owner_grant() {
        let name = format!(r"\\.\pipe\sot-access-test-{}", std::process::id());
        let listener = bind_session(&name).unwrap();
        let denied = restricted_open_denied(name.clone());
        assert_eq!(
            denied.kind(),
            std::io::ErrorKind::PermissionDenied,
            "restricted-token open was not denied: {denied}"
        );
        let no_accept =
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await;
        assert!(
            no_accept.is_err(),
            "denied token reached the connection boundary"
        );
        let client = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .expect("ordinary owner opens the live pipe");
        let stream = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut handlers = 0;
        assert!(dispatch_admitted(stream, admit_peer, |stream, _| {
            handlers += 1;
            drop(stream);
        }));
        assert_eq!(handlers, 1, "owner control did not dispatch");
        drop(client);
        eprintln!("admission-proof test=server::listen::tests::session_pipe_denies_a_token_without_the_owner_grant endpoint=session boundary=pipe-access fixture=restricted-token rejected=true dispatched=0 bodies=1");
    }
}
