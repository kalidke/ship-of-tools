use super::*;

#[cfg(unix)]
#[tokio::test]
async fn the_session_loop_survives_every_transient_and_resource_accept_error() {
    for code in [
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOBUFS,
        libc::ENOMEM,
        libc::ECONNABORTED,
        libc::EINTR,
        libc::EAGAIN,
    ] {
        survive_accept_error(std::io::Error::from_raw_os_error(code))
            .await
            .unwrap_or_else(|e| panic!("errno {code} ended the session loop: {e:#}"));
    }
    assert!(survive_accept_error(std::io::Error::from_raw_os_error(libc::EBADF)).await.is_err());
}
#[cfg(windows)]
use interprocess::local_socket::{GenericFilePath, ListenerOptions};

/// A fresh private folder and a socket path in it, short enough for macOS's `sun_path`.
#[cfg(unix)]
fn probe_socket_path() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("sotls-")
        .tempdir_in("/tmp")
        .expect("a folder under /tmp");
    let path = dir.path().join("s.sock");
    (dir, path)
}

/// A socket bound at `path` that never listens. A connect to its file is refused while the socket is open, and
/// after: no copy of it, in this process or in a child that inherited one, makes the file a listener's.
#[cfg(unix)]
fn unlistened_socket(path: &std::path::Path) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let kind = libc::SOCK_STREAM;
    // SAFETY: plain syscalls on a socket this function creates and returns.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, kind, 0);
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        let socket = std::os::fd::OwnedFd::from_raw_fd(fd);
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (dst, &b) in addr.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
            *dst = b as libc::c_char;
        }
        let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let bound = libc::bind(fd, std::ptr::addr_of!(addr).cast(), len);
        assert_eq!(bound, 0, "bind: {}", std::io::Error::last_os_error());
        socket
    }
}

/// The daemon's start probes read a missing socket and a stale socket file (no socket listens on it) as nothing
/// listening, so a crashed daemon's leftover does not block a restart, and a listener this account serves as one.
/// The stale file is a socket that never listened, not the listener after its drop: this test binary's other tests
/// start children, and a child forked while the listener is open holds it, listening, until its exec.
#[cfg(unix)]
#[test]
fn the_start_probes_tell_missing_and_stale_from_live() {
    let (dir, path) = probe_socket_path();
    assert!(
        refuse_live_socket(&path).is_ok(),
        "a missing socket is not a listener"
    );
    assert!(!socket_answers(&path));
    let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let refusal = refuse_live_socket(&path)
        .expect_err("a live listener refuses the start")
        .to_string();
    assert!(
        refusal.contains("another daemon is already listening on"),
        "{refusal}"
    );
    assert!(socket_answers(&path));
    let stale = dir.path().join("t.sock");
    let socket = unlistened_socket(&stale);
    assert!(
        refuse_live_socket(&stale).is_ok(),
        "a socket file nobody listens on is not a listener while its socket is open"
    );
    assert!(!socket_answers(&stale));
    drop(socket);
    assert!(stale.exists(), "the stale socket file is still there");
    assert!(
        refuse_live_socket(&stale).is_ok(),
        "a stale socket file is not a listener"
    );
    assert!(!socket_answers(&stale));
}

/// One nonblocking `connect(2)` to `path`: a queued connection is returned; `None` is the full-backlog `EAGAIN`.
#[cfg(target_os = "linux")]
fn nonblocking_connect(path: &std::path::Path) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: plain syscalls on a socket this function creates and either returns or closes.
    unsafe {
        let fd = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        );
        assert!(fd >= 0, "socket: {}", std::io::Error::last_os_error());
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_os_str().as_bytes();
        for (dst, &b) in addr.sun_path.iter_mut().zip(bytes) {
            *dst = b as libc::c_char;
        }
        let len =
            (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
        if libc::connect(fd, std::ptr::addr_of!(addr).cast(), len) == 0 {
            return Some(std::os::fd::OwnedFd::from_raw_fd(fd));
        }
        let err = std::io::Error::last_os_error();
        libc::close(fd);
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EAGAIN),
            "connect while filling the backlog: {err}"
        );
        None
    }
}

/// ADR 0049 `## User isolation`: a start probe of a socket this account listens on whose backlog is full returns
/// within `CONNECT_BOUND` plus slack instead of hanging, and refuses the start (`refuse_live_socket`) and counts as
/// an answer (`socket_answers`). Linux: a full backlog makes a connect wait there, where macOS refuses it at once.
#[cfg(target_os = "linux")]
#[test]
fn a_start_probe_of_a_full_backlog_returns_within_the_bound_and_refuses() {
    use std::os::fd::AsRawFd;
    let (_dir, path) = probe_socket_path();
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    // SAFETY: `listener` owns a valid listening socket for this call; `listen(2)` sets its backlog again.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
    let mut queued = Vec::new();
    while let Some(fd) = nonblocking_connect(&path) {
        queued.push(fd);
        assert!(queued.len() < 64, "the backlog never filled");
    }
    let probe_path = path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send((
            refuse_live_socket(&probe_path).map_err(|e| e.to_string()),
            socket_answers(&probe_path),
        ));
    });
    let bound = sot_log::lane::transport::CONNECT_BOUND * 2 + std::time::Duration::from_secs(3);
    let (refusal, answers) = rx
        .recv_timeout(bound)
        .expect("a start probe did not return within its bound");
    let refusal = refusal.expect_err("a full backlog refuses the start");
    assert!(
        refusal.contains("cannot tell whether a daemon is listening on"),
        "{refusal}"
    );
    assert!(answers, "a listener with a full backlog answers");
}

/// ADR 0049 `## User isolation`: a Unix connection is admitted only when its kernel-recorded effective uid is this
/// account's; foreign or missing records are refused. On Linux and macOS this is the account recorded at the
/// connect, not the holder's current euid; on macOS the pid is the live token's observation.
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
    let peer = admit_peer(&stream)
        .expect("this process is admitted")
        .process
        .expect("with its process");
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

/// Set only in a child of this test binary, to the session socket or pipe it connects to as a peer.
#[cfg(any(target_os = "linux", windows))]
const ADMISSION_PEER: &str = "SOT_TEST_ADMISSION_PEER";

/// The process `dispatch_admitted` handed `stream`'s handler under `admit_peer`, as text, or `None` when it
/// refused the stream.
#[cfg(target_os = "linux")]
fn admitted_process(stream: LocalStream) -> Option<String> {
    let mut process = None;
    dispatch_admitted(stream, admit_peer, |_, given| process = Some(format!("{given:?}")));
    process
}

/// ADR 0049 `## User isolation`: admission one decides the account, and the process a lease needs is read apart
/// from it. A connection whose recorded effective uid is this account's is admitted after its process has ended,
/// without a process, so it holds no lease.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_connection_of_this_account_whose_process_has_ended_is_admitted_without_it() {
    const NAME: &str =
        "server::listen::tests::a_connection_of_this_account_whose_process_has_ended_is_admitted_without_it";
    if let Some(path) = std::env::var_os(ADMISSION_PEER) {
        sot_log::test_isolated::enter(NAME);
        std::os::unix::net::UnixStream::connect(path).expect("connect to the parent's socket");
        return;
    }
    let (_dir, path) = probe_socket_path();
    let listener = bind_session(path.to_str().unwrap()).unwrap();
    let (mut command, entry) = sot_log::test_isolated::test_command(NAME);
    let mut child = command
        .env(ADMISSION_PEER, &path)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("start the connecting child");
    let pid = child.id();
    let status =
        sot_log::test_isolated::wait_within(&mut child, sot_log::test_isolated::ISOLATION_TIMEOUT);
    entry.assert_once(pid);
    assert!(status.success(), "the connecting child: {status}");
    assert!(
        sot_log::identity::challenge::process_created(pid).is_err(),
        "premise: the connecting child's process is gone"
    );
    let stream = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
        .await
        .expect("the child's connection is queued")
        .expect("accept");
    assert_eq!(
        admitted_process(stream).as_deref(),
        Some("None"),
        "a connection of this account whose process has ended is admitted, without its process"
    );
}

/// ADR 0049 `## User isolation`: on Windows the pipe's owner-only descriptor decides the account, so a process of
/// this account whose process object this account may not open is admitted, without its process, so it holds no
/// lease. The child grants its object to SYSTEM alone, as an administrator's ssh session grants it only to
/// Administrators, SYSTEM and its own logon, which the daemon's filtered token is none of.
#[cfg(windows)]
#[tokio::test]
async fn a_process_of_this_account_this_daemon_may_not_open_is_admitted_without_it() {
    const NAME: &str =
        "server::listen::tests::a_process_of_this_account_this_daemon_may_not_open_is_admitted_without_it";
    if let Some(pipe) = std::env::var_os(ADMISSION_PEER) {
        sot_log::test_isolated::enter(NAME);
        return closed_peer(&pipe);
    }
    let name = format!(r"\\.\pipe\sot-closed-peer-test-{}", std::process::id());
    let listener = bind_session(&name).unwrap();
    let (mut command, entry) = sot_log::test_isolated::test_command(NAME);
    let mut child = command
        .env(ADMISSION_PEER, &name)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("start the peer child");
    let pid = child.id();
    let accepted = tokio::time::timeout(std::time::Duration::from_secs(20), listener.accept()).await;
    // Both reads run as the daemon's token would: no privilege, so none of the runner's reaches the child.
    let denied = without_privileges(|| sot_log::identity::challenge::process_created(pid));
    let mut held = None;
    let process = match accepted {
        Ok(Ok(stream)) => without_privileges(|| {
            let mut process = None;
            dispatch_admitted(stream, admit_peer, |stream, given| {
                process = Some(format!("{given:?}"));
                held = Some(stream);
            });
            Some(process)
        }),
        _ => None,
    };
    drop(listener);
    // The child ends on this byte: the synchronous wait below blocks this runtime, so the peer's read is not
    // left to see the stream close.
    if let Some(mut stream) = held {
        use tokio::io::AsyncWriteExt;
        stream.write_all(&[0]).await.expect("release the peer child");
    }
    let status =
        sot_log::test_isolated::wait_within(&mut child, sot_log::test_isolated::ISOLATION_TIMEOUT);
    entry.assert_once(pid);
    assert!(status.success(), "the peer child: {status}");
    let denied = denied.expect_err("premise: this account may not open the peer child's process");
    assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied, "premise: {denied}");
    assert_eq!(
        process,
        Some(Some("None".to_string())),
        "a process of this account this daemon may not open is admitted, without its process"
    );
}

/// The peer child of the test above: grants its own process object to SYSTEM alone, then opens `pipe` and holds it
/// until the parent writes a byte or five seconds pass.
#[cfg(windows)]
fn closed_peer(pipe: &std::ffi::OsStr) {
    use std::io::Read;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let sddl = sot_log::host::wide_null("D:P(A;;GA;;;SY)");
    // SAFETY: the descriptor is built, read and freed here; the process handle is this process's pseudo-handle.
    unsafe {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let built = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        );
        assert_ne!(built, 0, "build the descriptor: {}", std::io::Error::last_os_error());
        let (mut present, mut dacl, mut defaulted) = (0, std::ptr::null_mut::<ACL>(), 0);
        assert_ne!(GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted), 0, "its DACL");
        let set = SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        );
        LocalFree(sd as _);
        assert_eq!(set, 0, "grant this process's object to SYSTEM alone");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut opened = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(pipe) {
            Err(e) if e.raw_os_error() == Some(231) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            opened => break opened.expect("open the parent's pipe"),
        }
    };
    // A refused stream is dropped with no byte written, so the read gets a deadline of its own and the parent's
    // assertion, not the isolation bound, reports a refusal.
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = opened.read(&mut [0u8; 1]);
        let _ = done.send(());
    });
    let _ = waited.recv_timeout(std::time::Duration::from_secs(5));
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
/// A token handle closed on drop.
#[cfg(windows)]
struct Token(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Ends the impersonation of the thread that made it, on drop.
#[cfg(windows)]
struct Restore;
#[cfg(windows)]
impl Drop for Restore {
    fn drop(&mut self) {
        assert_ne!(
            unsafe { windows_sys::Win32::Security::RevertToSelf() },
            0,
            "restore test-owned token"
        );
    }
}

/// This thread, impersonating a restricted copy of this process's token until the guard drops. `flags` go to
/// `CreateRestrictedToken`; `world_only` adds the Everyone SID as the one restricting SID.
#[cfg(windows)]
fn impersonate_restricted(flags: u32, world_only: bool) -> Restore {
    use windows_sys::Win32::Security::{
        CreateRestrictedToken, CreateWellKnownSid, ImpersonateLoggedOnUser, WinWorldSid, SID_AND_ATTRIBUTES,
        TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
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
        unsafe { CreateWellKnownSid(WinWorldSid, std::ptr::null_mut(), sid.as_mut_ptr().cast(), &mut bytes) },
        0,
        "create restricted access SID"
    );
    let restriction = SID_AND_ATTRIBUTES { Sid: sid.as_mut_ptr().cast(), Attributes: 0 };
    let mut restricted = Token(std::ptr::null_mut());
    assert_ne!(
        unsafe {
            CreateRestrictedToken(
                original.0,
                flags,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                u32::from(world_only),
                if world_only { &restriction } else { std::ptr::null() },
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
    Restore
}

/// Runs `read` on this thread under a copy of its token with every privilege removed, as the daemon's filtered
/// token has none, whatever the runner holds (an administrator's ssh session holds SeDebugPrivilege, which
/// ignores a process object's DACL). `read` is synchronous: impersonation never crosses an await.
#[cfg(windows)]
fn without_privileges<R>(read: impl FnOnce() -> R) -> R {
    use windows_sys::Win32::Security::DISABLE_MAX_PRIVILEGE;
    let _restore = impersonate_restricted(DISABLE_MAX_PRIVILEGE, false);
    read()
}

#[cfg(windows)]
/// Native access check on a synchronous test-owned client thread; impersonation never crosses an await.
fn restricted_open_denied(path: String) -> std::io::Error {
    std::thread::spawn(move || {
        let _restore = impersonate_restricted(0, true);
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
