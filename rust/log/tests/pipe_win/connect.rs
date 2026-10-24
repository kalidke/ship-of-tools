//! Connect tests: an absent pipe fails fast, a busy one is retried within the bound, and a token without the owner grant
//! is refused by the row pipe.

use super::*;
use sot_platform::transport::connect_own::connect_own;
use sot_platform::transport::{TransportError, CONNECT_BOUND};

/// ADR 0043 decision 27: an ABSENT pipe, with no instance created for this name, fails the connect on the FIRST
/// `CreateFileW` attempt with `ERROR_FILE_NOT_FOUND`, never retried within [`CONNECT_BOUND`]: absence is the caller's
/// to poll, not this bound's to spend. Elapsed time is asserted GENEROUSLY (the bound itself), evidence the bound was
/// never consumed, not a tight perf gate.
#[test]
fn connect_fails_fast_when_pipe_absent() {
    if !run_isolated("connect::connect_fails_fast_when_pipe_absent") {
        return;
    }
    let path = fresh_row_pipe(); // nothing ever binds this name

    let started = Instant::now();
    let err = connect_own(&path).unwrap_err();
    let elapsed = started.elapsed();

    eprintln!("connect_fails_fast_when_pipe_absent: elapsed={elapsed:?}");
    assert!(err.is_endpoint_absent(), "expected an absent endpoint, got {err}");
    assert!(
        matches!(err, TransportError::Io { .. }),
        "expected the connector's own Io error, got {err}"
    );
    assert!(
        elapsed < CONNECT_BOUND,
        "an absent pipe (ERROR_FILE_NOT_FOUND) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

/// ADR 0043 decision 27: a BUSY pipe (every instance already claimed) is a DIFFERENT case from an absent one, and stays
/// retried within [`CONNECT_BOUND`]. A pipe with ONE instance stands in: one client holds it, a second connect starts
/// on its own thread, and the test asserts it is STILL PENDING after 300 ms (busy, not failed). The server then frees
/// the instance (disconnect, then listen again, as the row's listener does between clients), and the second connect
/// succeeds once the instance is available again.
#[test]
fn connect_retries_within_the_bound_when_busy_then_succeeds_once_freed() {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    if !run_isolated("connect::connect_retries_within_the_bound_when_busy_then_succeeds_once_freed") {
        return;
    }
    let path = fresh_row_pipe();
    let name = wide_null(path.to_str().expect("a pipe name"));
    let instance = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            1,
            512,
            512,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(instance, INVALID_HANDLE_VALUE, "CreateNamedPipeW failed: {}", std::io::Error::last_os_error());
    let instance = Handle(instance);

    let first_client = connect_own(&path).unwrap();

    // The connector sends its pre-call origin; wait only until origin + 300 ms before freeing the instance.
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let path_for_thread = path.clone();
    let second = std::thread::spawn(move || {
        let started = Instant::now();
        let _ = started_tx.send(started);
        let client = connect_own(&path_for_thread).expect("expected the busy retry to eventually succeed");
        (client, started.elapsed())
    });
    let started = started_rx.recv_timeout(TIMEOUT).expect("expected the second connect thread to signal it has started");
    std::thread::sleep((started + Duration::from_millis(300)).saturating_duration_since(Instant::now()));
    assert!(!second.is_finished(), "expected the second connect to still be retrying against a busy pipe 300ms after it started");

    // Free the instance: disconnect the first client, then listen again (a blocking call, on its own thread).
    drop(first_client);
    unsafe { DisconnectNamedPipe(instance.0) };
    let raw = instance.0 as usize;
    let listen = std::thread::spawn(move || unsafe {
        ConnectNamedPipe(raw as windows_sys::Win32::Foundation::HANDLE, std::ptr::null_mut());
    });

    let join_deadline = Instant::now() + CONNECT_BOUND + Duration::from_secs(5);
    while !second.is_finished() {
        assert!(Instant::now() < join_deadline, "expected the second connect thread to finish once the instance was freed");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_second_client, elapsed) = second.join().unwrap();
    eprintln!("connect_retries_within_the_bound_when_busy_then_succeeds_once_freed: elapsed={elapsed:?}");
    assert!(elapsed < CONNECT_BOUND, "expected the busy retry to succeed comfortably inside {CONNECT_BOUND:?}, took {elapsed:?}");
    let _ = listen.join();
}

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

/// A token without the owner grant is refused by the row pipe, repeatedly, and takes no connection; the owner's token,
/// on the same instance, is served the hello, and the instance's descriptor is unchanged afterward.
#[test]
fn pipe_denies_a_token_without_the_owner_grant() {
    use std::io::{BufRead, Write};
    let test = "connect::pipe_denies_a_token_without_the_owner_grant";
    if !run_isolated(test) {
        return;
    }
    let (path, server, run) = bind_row();
    let path_text = path.to_str().expect("a pipe name").to_string();
    for _ in 0..2 {
        let denied = restricted_open_denied(path_text.clone());
        assert_eq!(
            denied.kind(),
            std::io::ErrorKind::PermissionDenied,
            "restricted-token open was not access denied: {denied}"
        );
        assert_eq!(run.open_connections(), 0, "a denied token took a connection");
        let deadline = Instant::now() + TIMEOUT;
        let mut owner = loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => break file,
                Err(error) if error.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("owner-token open failed: {error}"),
            }
        };
        let hello = sot_protocol::Frame::req(1, "row.hello", serde_json::json!({"build": "t", "row": sot_log::row::ROW_PROTOCOL}));
        let mut text = serde_json::to_vec(&hello).unwrap();
        text.push(b'\n');
        owner.write_all(&text).unwrap();
        let (owner, reply) = sot_platform::test_wait::within(TIMEOUT, "the owner's hello reply", move || {
            let mut line = String::new();
            let mut reader = std::io::BufReader::new(&mut owner);
            reader.read_line(&mut line).unwrap();
            drop(reader);
            (owner, line)
        });
        let reply: sot_protocol::Frame = serde_json::from_str(reply.trim_end()).expect("a hello reply");
        assert_eq!(reply.op, "row.hello", "the owner is served");
        let actual = security_descriptor_sddl(std::os::windows::io::AsRawHandle::as_raw_handle(&owner).cast());
        let expected = canonical_sddl(&format!("D:P(A;;FA;;;{})", current_user_sid_string()));
        assert_eq!(actual, expected, "live endpoint descriptor changed");
        drop(owner);
        let deadline = Instant::now() + TIMEOUT;
        while run.open_connections() != 0 {
            assert!(Instant::now() < deadline, "the owner's connection never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("admission-proof test={test} endpoint=row boundary=pipe-access fixture=restricted-token rejected=true dispatched=0 bodies=1");
    }
    server.shutdown();
}
