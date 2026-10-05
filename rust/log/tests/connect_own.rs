//! `connect_own`, the one rule for a local endpoint reached by name: a client speaks to this box's daemon socket or pipe
//! only when this OS account serves it. Unix tests pin the folder check; Windows tests pin the serving-process check.

use sot_log::identity::connect_own::connect_own;
use sot_log::lane::transport::TransportError;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

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

    /// ADR 0049, User isolation: a socket whose folder another account could write in may be another account's; the
    /// client refuses it before it connects, so the listener never sees a connection.
    #[test]
    fn a_socket_whose_folder_is_not_private_is_refused() {
        let (dir, path, listener) = listener_in_folder(0o755);
        listener.set_nonblocking(true).unwrap();
        let err = match connect_own(&path) {
            Ok(_) => panic!("connected to {}", path.display()),
            Err(e) => e,
        };
        let TransportError::Io { op, source } = err else {
            panic!("not an Io error: {err}");
        };
        assert_eq!(op, "connect_own");
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            source.to_string(),
            format!(
                "{}: not connecting: {} is not a private folder of this OS account",
                path.display(),
                dir.display()
            )
        );
        match listener.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("the listener saw a connection: {other:?}"),
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ADR 0049, User isolation: the same socket in a private folder of this account is connected to.
    #[test]
    fn a_socket_in_a_private_folder_is_accepted() {
        let (dir, path, listener) = listener_in_folder(0o700);
        let _client = connect_own(&path).unwrap_or_else(|e| panic!("refused {}: {e}", path.display()));
        listener.accept().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ADR 0049, User isolation: a folder that is missing keeps the kind a connect to a missing socket gives, so a
    /// caller that treats "no daemon yet" as NotFound is unchanged.
    #[test]
    fn a_missing_folder_is_not_found() {
        let path = std::env::temp_dir().join(format!("sot-co-{}-missing", std::process::id())).join("s.sock");
        let TransportError::Io { source, .. } = connect_own(&path).err().expect("refused") else {
            panic!("not an Io error");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        assert!(source.to_string().contains("does not exist"), "{source}");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use sot_log::host::wide_null;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
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
}

/// Files that keep their own stream type and so apply the rule around their one dial; each must contain the names listed.
/// Every connector rust/clippy.toml names is a compile error in rust.yml's "Local endpoint dials" step unless it sits at
/// an `#[allow]` with its reason; the allow at these two sites is what this test holds to the rule. It checks that the
/// names appear on a non-comment line of the file: not their order, not that they sit in the dialing function, and not
/// any other file. `std::fs::OpenOptions::open` cannot be disallowed, since it opens every file, so a third opener of a
/// pipe path would pass both checks.
const WRAPPED: &[(&str, &[&str])] = &[
    // `connect_pipe`: the window's transport and lease dial; its Windows arm goes through `connect_own`.
    ("frontend/src/net/transport/mod.rs", &["own_socket(", "connect_own("]),
    // `connect`: `sotd topology`'s dial; its `pipe:` arm opens a file.
    ("backend/src/topology/dial.rs", &["own_socket(", "own_pipe("]),
];

/// ADR 0049, User isolation: the two files whose dial the lint allows, and the one whose pipe the lint cannot see, still
/// call the rule on both platforms (both the Unix and the Windows arm sit in the source text).
#[test]
fn the_files_that_wrap_their_own_dial_call_the_rule() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut failures = Vec::new();
    for (rel, rules) in WRAPPED {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for rule in *rules {
            if !src.lines().any(|l| !l.trim_start().starts_with("//") && l.contains(rule)) {
                failures.push(format!("{rel}: never calls {rule}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
