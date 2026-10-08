//! `connect_own`, the one rule for a local endpoint reached by name: a client speaks to this box's daemon socket or pipe
//! only when this OS account serves it. Unix tests pin the listener's account and the connect's retry budget; Windows
//! tests pin the serving-process check and the connect's retry budget.

use sot_log::identity::connect_own::connect_own;
use sot_log::lane::transport::TransportError;

#[cfg(unix)]
mod unix {
    use super::*;
    use sot_log::lane::transport::CONNECT_BOUND;
    use sot_log::test_foreign::ForeignListener;
    use sot_log::test_isolated::run_isolated;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A fresh folder at the mode given, with a socket bound in it. Short path: macOS's `sun_path` is 104 bytes.
    fn listener_in_folder(mode: u32) -> (PathBuf, PathBuf, UnixListener) {
        let dir = std::env::temp_dir().join(format!("sot-co-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path, listener)
    }

    /// ADR 0049, User isolation: a socket this account listens on is reached in a folder every account can read: the
    /// check is the listener's account, not the folder's mode.
    #[test]
    fn a_socket_this_account_listens_on_is_reached_wherever_it_is() {
        let (dir, path, listener) = listener_in_folder(0o755);
        let _client =
            connect_own(&path).unwrap_or_else(|e| panic!("refused {}: {e}", path.display()));
        listener.accept().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ADR 0049, User isolation: a socket another account listens on, in a folder private to this account, is
    /// refused before a byte is written, by the account the kernel recorded at `listen()`.
    #[test]
    fn a_socket_another_account_listens_on_is_refused() {
        if !run_isolated("unix::a_socket_another_account_listens_on_is_refused") {
            return;
        }
        let Some(foreign) = ForeignListener::start(false) else {
            return;
        };
        let err = match connect_own(&foreign.path) {
            Ok(_) => panic!("connected to {}", foreign.path.display()),
            Err(e) => e,
        };
        let TransportError::Io { op, source } = err else {
            panic!("not an Io error: {err}");
        };
        assert_eq!(op, "connect_own", "{source}");
        assert_eq!(
            source.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{source}"
        );
        assert_eq!(
            source.to_string(),
            format!(
                "{}: not connecting: another OS account listens on this socket",
                foreign.path.display()
            )
        );
        assert_eq!(
            foreign.finish(),
            0,
            "connect_own sent another account's listener bytes"
        );
    }

    /// ADR 0049, User isolation: a connect to a socket whose backlog another account has filled returns within
    /// `CONNECT_BOUND` plus 2 s of slack without connecting, so no caller of `connect_own` hangs on it.
    #[test]
    fn a_full_backlog_ends_the_connect_within_its_bound() {
        if !run_isolated("unix::a_full_backlog_ends_the_connect_within_its_bound") {
            return;
        }
        let Some(foreign) = ForeignListener::start(true) else {
            return;
        };
        let started = Instant::now();
        let err = connect_own(&foreign.path)
            .err()
            .expect("connected through a full backlog");
        assert!(
            started.elapsed() < CONNECT_BOUND + Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        let TransportError::Io { op, source } = err else {
            panic!("not an Io error: {err}");
        };
        assert_ne!(
            op, "connect_own",
            "the connect went through, so the backlog was not full: {source}"
        );
        assert_eq!(
            foreign.finish(),
            0,
            "connect_own sent another account's listener bytes"
        );
    }

    /// A missing socket keeps the connector's NotFound, so a caller that treats "no daemon yet" as NotFound is unchanged.
    #[test]
    fn a_missing_socket_is_not_found() {
        let path = PathBuf::from(format!("/tmp/sot-co-{}-missing.sock", std::process::id()));
        let TransportError::Io { source, .. } = connect_own(&path).err().expect("refused") else {
            panic!("not an Io error");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound, "{source}");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use sot_log::host::wide_null;
    use sot_log::lane::transport::CONNECT_BOUND;
    use sot_log::test_isolated::run_isolated;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A pipe some system service serves under its own account, which no user account runs: the stand-in for another
    /// account's pipe (a squatter's pipe is refused by the same two OS calls).
    const SYSTEM_PIPE: &str = r"\\.\pipe\epmapper";

    /// ADR 0049, User isolation: a pipe that another account serves is refused before a byte is written.
    #[test]
    fn a_pipe_served_by_another_account_is_refused() {
        let path = PathBuf::from(SYSTEM_PIPE);
        let err = match connect_own(&path) {
            Ok(_) => panic!("connected to {}", path.display()),
            Err(e) => e,
        };
        let TransportError::Io { op, source } = err else {
            panic!("not an Io error: {err}");
        };
        assert_eq!(op, "connect_own", "{source}");
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied, "{source}");
        let text = source.to_string();
        assert!(text.starts_with(SYSTEM_PIPE), "{text}");
        assert!(text.contains("not connecting"), "{text}");
    }

    /// ADR 0049, User isolation: the pipe is opened at identification level, so a server that impersonates the client
    /// right after the first byte gets an identification token and cannot act as the account.
    #[test]
    fn the_pipe_is_opened_at_identification_level() {
        use windows_sys::Win32::Security::SecurityIdentification;
        let level = sot_log::identity::impersonation_probe::level_seen_by_server(|name| {
            let client = connect_own(std::path::Path::new(name)).unwrap_or_else(|e| panic!("refused {name}: {e}"));
            client.write_all(b"x").expect("write one byte");
            client
        });
        assert_eq!(level, SecurityIdentification);
    }

    /// ADR 0049, User isolation: a pipe this very process serves, so this account does, is connected to.
    #[test]
    fn a_pipe_this_account_serves_is_accepted() {
        let name = format!(r"\\.\pipe\sot-connect-own-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
        let wide = wide_null(&name);
        let server = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(server, INVALID_HANDLE_VALUE, "{}", std::io::Error::last_os_error());
        let result = connect_own(&PathBuf::from(&name));
        unsafe { CloseHandle(server) };
        result.unwrap_or_else(|e| panic!("refused {name}: {e}"));
    }

    /// ADR 0049, User isolation: a pipe whose one instance is taken returns from the connect within `CONNECT_BOUND` plus
    /// 2 s of slack, so no caller of `connect_own` hangs on it.
    #[test]
    fn a_busy_pipe_ends_the_connect_within_its_bound() {
        if !run_isolated("windows::a_busy_pipe_ends_the_connect_within_its_bound") {
            return;
        }
        let name = format!(
            r"\\.\pipe\sot-connect-own-busy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let wide = wide_null(&name);
        let server = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(
            server,
            INVALID_HANDLE_VALUE,
            "{}",
            std::io::Error::last_os_error()
        );
        let path = PathBuf::from(&name);
        let first = connect_own(&path);
        let started = Instant::now();
        let second = connect_own(&path);
        let took = started.elapsed();
        unsafe { CloseHandle(server) };
        first.unwrap_or_else(|e| panic!("refused {name}: {e}"));
        assert!(
            second.is_err(),
            "connected to a pipe whose one instance is taken"
        );
        assert!(
            took < CONNECT_BOUND + Duration::from_secs(2),
            "took {took:?}"
        );
    }
}
