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

/// The dial primitives: each reaches a socket or pipe by name with no check of who serves it.
const DIAL_PRIMITIVES: &[&str] = &[
    "UnixStream::connect(",
    "LocalStream::connect(",
    "local_socket::tokio::Stream::connect(",
    "connect_unix_socket_unchallenged(",
    "connect_pipe_path_unchallenged(",
    "connect_named_pipe_unchallenged(",
];

/// Files that may hold any number of primitives, each with its reason.
const FREE: &[&str] = &[
    // The rule itself: the platform connector, then the check.
    "log/src/identity/connect_own.rs",
    // The connectors; their own callers in these folders run the identity challenge after connecting.
    "log/src/lane/socket_unix/client.rs",
    "log/src/lane/pipe_win/client.rs",
    // The daemon probing the path it is about to bind: an answer refuses the start, nothing is written.
    "backend/src/server/listen.rs",
];

/// Files that keep their own stream type and so apply the rule around their one primitive: exactly one, and the file
/// calls both `own_socket(` and `own_pipe(`.
const WRAPPED: &[&str] = &[
    // `connect_pipe`: an interprocess tokio stream the window's transport and lease share.
    "frontend/src/net/transport/mod.rs",
    // `connect`: std streams the dial clones. Its `pipe:` arm opens a file, which no text search can tell from any other
    // open, so this wrapped-file check (both rule calls present) is what holds that arm to the rule.
    "backend/src/topology/dial.rs",
];

/// A cfg predicate that holds only when `test` is set: `test`, or `all(..)` with such an operand.
fn requires_test(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" {
        return true;
    }
    let Some(inner) = pred.strip_prefix("all(").and_then(|p| p.strip_suffix(')')) else {
        return false;
    };
    let (mut depth, mut cur, mut operands) = (0i32, String::new(), Vec::new());
    for ch in inner.chars() {
        if ch == ',' && depth == 0 {
            operands.push(std::mem::take(&mut cur));
            continue;
        }
        depth += i32::from(ch == '(') - i32::from(ch == ')');
        cur.push(ch);
    }
    operands.push(cur);
    operands.iter().any(|o| requires_test(o))
}

/// The lines of `src` outside test code: a block opening with a column-0 `#[cfg(..)]` that requires `test`, followed by a
/// `mod` line, runs to the next line that is exactly `}` (or is that one `mod ...;` line).
fn non_test_lines(src: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut kept = Vec::new();
    let mut k = 0;
    while k < lines.len() {
        let is_test_cfg = |l: &str| {
            l.strip_prefix("#[cfg(")
                .and_then(|r| r.split_once(")]").map(|(p, _)| p))
                .is_some_and(requires_test)
        };
        if lines[k].starts_with("#[cfg(") && is_test_cfg(lines[k]) {
            let mut j = k + 1;
            while j < lines.len() && (lines[j].trim().is_empty() || lines[j].trim_start().starts_with("#[")) {
                j += 1;
            }
            if j < lines.len() && (lines[j].starts_with("mod ") || lines[j].starts_with("pub mod ")) {
                let mut end = j;
                if !lines[j].trim_end().ends_with(';') {
                    end = j + 1;
                    while end < lines.len() && lines[end] != "}" {
                        end += 1;
                    }
                }
                k = end + 1;
                continue;
            }
        }
        kept.push((k + 1, lines[k]));
        k += 1;
    }
    kept
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap();
    rel.split('/').rev().skip(1).any(|d| d == "tests")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
}

/// ADR 0049, User isolation: a Rust client that dials this box's daemon or a relay socket by name does it through the
/// rule. Every dial primitive in non-test code under the five crates sits in a file this test lists, free (the rule, the
/// connectors, the daemon's own probe) or wrapped (its one primitive, with `own_socket(` and `own_pipe(` both called).
/// A primitive anywhere else fails here, naming file:line; so does a listed file that has lost its rule.
#[test]
fn no_local_dial_outside_connect_own() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    for krate in ["backend", "frontend", "log", "protocol", "updater"] {
        rust_files(&root.join(krate).join("src"), &mut files);
    }
    assert!(files.len() >= 100, "found only {} source files", files.len());

    let mut found_free = Vec::new();
    let mut failures = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        let hits: Vec<(usize, &str)> = non_test_lines(&src)
            .into_iter()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| DIAL_PRIMITIVES.iter().any(|p| l.contains(p)))
            .collect();
        if hits.is_empty() && !WRAPPED.contains(&rel.as_str()) {
            continue;
        }
        if FREE.contains(&rel.as_str()) {
            found_free.push(rel);
        } else if WRAPPED.contains(&rel.as_str()) {
            if hits.len() != 1 {
                failures.push(format!("{rel}: {} dial primitives, wrapped files hold exactly one", hits.len()));
            }
            for rule in ["own_socket(", "own_pipe("] {
                if !non_test_lines(&src).iter().any(|(_, l)| l.contains(rule)) {
                    failures.push(format!("{rel}: never calls {rule}"));
                }
            }
        } else {
            for (n, l) in hits {
                failures.push(format!("{rel}:{n}: dial primitive outside connect_own: {}", l.trim()));
            }
        }
    }
    for listed in FREE {
        assert!(found_free.iter().any(|f| f == listed), "{listed} is listed free but holds no dial primitive");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
