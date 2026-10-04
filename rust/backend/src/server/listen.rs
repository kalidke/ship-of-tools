//! The listener: the daemon lock, the live-socket refusal and the local-socket accept loop.

use super::*;
use super::conn::handle_connection;

use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath, ListenerOptions,
};
// Session-pipe hardening fix: on Windows the local-socket name above is a
// named pipe, and `interprocess` leaves an unset security descriptor at its
// platform default (`Everyone`/`ANONYMOUS LOGON` read) unless one is
// supplied through this extension trait — see `run_local`'s
// `session_pipe_security_descriptor`.
#[cfg(windows)]
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt,
    security_descriptor::{AsSecurityDescriptorExt, BorrowedSecurityDescriptor},
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
) -> Result<sot_log::fence::DaemonLock> {
    let lock_path = sot_log::fence::daemon_lock_path(state_root);
    let deadline = tokio::time::Instant::now() + daemon_lock_wait();
    let mut logged = false;
    loop {
        if let Some(lock) = sot_log::fence::try_lock_daemon(state_root)
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

/// Whether a daemon answers on the session socket: on Unix a connect
/// succeeds, on Windows a client open of the pipe name succeeds.
fn socket_answers(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::net::UnixStream::connect(path).is_ok();
    #[cfg(windows)]
    return std::fs::OpenOptions::new().read(true).write(true).open(path).is_ok();
}

/// Refuses when a daemon still answers on the socket at `path`. Unlinking
/// a live daemon's socket leaves it running on a deleted file that no
/// client can reach until it restarts. Only a socket nobody answers on
/// (ECONNREFUSED), or none at all, is stale; any other connect error
/// refuses too, because it cannot tell.
#[cfg(unix)]
pub(crate) fn refuse_live_socket(path: &std::path::Path) -> Result<()> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => anyhow::bail!(
            "another daemon is already listening on {}; refusing to start on its socket \
             (stop that daemon first, or pass a different --socket)",
            path.display()
        ),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => Ok(()),
        Err(e) => anyhow::bail!(
            "cannot tell whether a daemon is listening on {}: {e}; refusing to unlink its socket",
            path.display()
        ),
    }
}

/// The session pipe's security descriptor: protected, owner-only full
/// access, no `OI`/`CI` inheritance — built from `sot_log::
/// owner_protected_pipe_descriptor` (the SAME SDDL `pipe_win.rs` already
/// uses for the voyage/supervisor pipes) rather than a second copy of that
/// string. `interprocess`'s own `ListenerOptions` has no ACL-building of
/// its own to reuse; `ListenerOptionsExt::security_descriptor` only takes
/// its crate's own `SecurityDescriptor` type, so this borrows the raw
/// descriptor `sot_log` built and clones it in (`to_owned_sd`) rather than
/// hand-rolling a second SDDL string here.
#[cfg(windows)]
fn session_pipe_security_descriptor(
) -> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    let descriptor =
        sot_log::owner_protected_pipe_descriptor().context("build session pipe security descriptor")?;
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

pub(super) async fn run_local(
    socket_path: PathBuf,
    session: Session,
    token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / handle_connection signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    preview_changed_tx: broadcast::Sender<PreviewChanged>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    agent_receipt_tx: broadcast::Sender<AgentReceipt>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    topology_store: Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology_store::TopologyChanged>,
    leases: Arc<crate::lease::Leases>,
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
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    #[allow(unused_mut)]
    let mut listener_options = ListenerOptions::new().name(name);
    // Windows only: every legitimate client (frontend, CLI, capsule agents,
    // the comm bridge) runs as this same OS user, so owner-only full access
    // is sufficient — same posture `pipe_win.rs` already gives the
    // voyage/supervisor pipes, applied here to the session pipe too. Unix
    // is unaffected: its socket security is the containing directory's mode
    // (`paths::secure_socket_dir` above), not this builder.
    #[cfg(windows)]
    {
        listener_options = listener_options.security_descriptor(session_pipe_security_descriptor()?);
    }
    let listener = listener_options
        .create_tokio()
        .with_context(|| format!("bind {socket_path:?}"))?;
    tracing::info!(socket = ?socket_path, "listening (local)");

    // The accept loop ends when a shutdown begins: shutdown step 1 stops
    // accepting by dropping the listener and unlinking the socket, on the
    // same wake as the deciding departure, before any row is touched.
    let decided = loop {
        let stream: LocalStream = tokio::select! {
            accepted = listener.accept() => accepted.context("accept on sot socket")?,
            () = leases.gone() => break tokio::time::Instant::now(),
        };
        let peer_identity = crate::lease::accepted_peer(&stream);
        let le = leases.clone();
        let s = session.clone();
        let tok = token.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
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
                rx, tx, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, agr, fce, rfe,
                cl, tps, tpe, "local", None, peer_identity, le,
            )
            .await
            {
                tracing::warn!(error = %e, transport = "local", "connection ended with error");
            } else {
                tracing::info!(transport = "local", "connection closed");
            }
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
    crate::shutdown::run(leases, workspaces, ws_events_tx, decided).await
}

#[cfg(test)]
mod tests {

    /// Twin of `sot-log`'s own
    /// `pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags`
    /// (`rust/log/tests/pipe_win.rs`) — same technique (`GetSecurityInfo` on
    /// a LIVE handle, round-tripped to SDDL) — but against THIS crate's
    /// session pipe rather than a voyage/supervisor pipe: proves `run_local`
    /// actually wires `session_pipe_security_descriptor()` into the
    /// `interprocess` listener, not merely that the descriptor builds
    /// correct bytes in isolation. Before this fix the session pipe carried
    /// the Windows default (`Everyone`/`ANONYMOUS LOGON` read).
    #[cfg(windows)]
    #[tokio::test]
    async fn session_pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
        use super::session_pipe_security_descriptor;
        use interprocess::local_socket::tokio::prelude::*;
        use interprocess::local_socket::{GenericFilePath, ListenerOptions};
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW,
            ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertSidToStringSidW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, READ_CONTROL,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        fn wide(s: &str) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
        }

        fn current_user_sid_string() -> String {
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                assert_ne!(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token), 0);
                let mut needed: u32 = 0;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
                assert!(needed > 0, "GetTokenInformation sizing call returned zero length");
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
                assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
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
            let wide_sddl = wide(sddl);
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
        let fs_name = name.as_str().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new()
            .name(fs_name)
            .security_descriptor(session_pipe_security_descriptor().unwrap())
            .create_tokio()
            .unwrap();

        let wide_name = wide(&name);
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
}
