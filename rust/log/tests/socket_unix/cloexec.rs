//! Descriptor flags of the lane factories, observed on the real listener, connector and wake pipe: close-on-exec at
//! birth where the platform creates it atomically (Linux), at publication everywhere, and a failed flag call returned
//! with every descriptor closed. No test claims the macOS creation-to-flagging window is closed.

use super::*;
use sot_log::lane::test_progress::{observe_births, Birth};

macro_rules! isolated {
    ($test:expr) => {
        if !named!(
            $test,
            "child.wait",
            "isolated body and bounded completion",
            None
        ) {
            return;
        }
    };
}

fn birth_of<'a>(births: &'a [Birth], role: &str) -> &'a Birth {
    births
        .iter()
        .find(|b| b.role == role)
        .unwrap_or_else(|| panic!("the factory never created a {role} descriptor: {births:?}"))
}

#[cfg(target_os = "linux")]
fn assert_born_close_on_exec(birth: &Birth) {
    assert!(
        birth.fd_flags >= 0 && birth.fd_flags & libc::FD_CLOEXEC != 0,
        "descriptor missing FD_CLOEXEC at birth: {birth:?}"
    );
}

fn assert_published_close_on_exec(birth: &Birth) {
    let now = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
    assert!(
        now >= 0 && now & libc::FD_CLOEXEC != 0,
        "{} was published without FD_CLOEXEC",
        birth.role
    );
}

fn assert_published_nonblocking(birth: &Birth) {
    let flags = unsafe { libc::fcntl(birth.fd, libc::F_GETFL) };
    assert!(
        flags >= 0 && flags & libc::O_NONBLOCK != 0,
        "{} was published without O_NONBLOCK",
        birth.role
    );
}

fn closed(fd: std::os::fd::RawFd) -> bool {
    let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
}

/// Bind a server under observation; the births are the factory's own.
fn bind_observed(
    test: &str,
    fail_at: Option<usize>,
) -> (
    Result<SocketServer, TransportError>,
    Vec<Birth>,
    usize,
    String,
) {
    let id = fresh_voyage_id();
    let (result, births, calls) = observe_births(fail_at, || {
        io_named!(
            test,
            "server.bind",
            "bind under observation",
            None,
            SocketServer::bind(&id, 2)
        )
    });
    (result, births, calls, id)
}

fn listener_birth(test: &str) {
    isolated!(test);
    let _rt = isolated_runtime_dir();
    let (server, births, _, _) = bind_observed(test, None);
    let _server = server.expect("the server binds");
    let birth = birth_of(&births, "listener");
    #[cfg(target_os = "linux")]
    assert_born_close_on_exec(birth);
    assert_published_close_on_exec(birth);
}

#[test]
fn listener_is_born_close_on_exec() {
    listener_birth("cloexec::listener_is_born_close_on_exec");
}

fn wake_birth(test: &str, role: &str) {
    isolated!(test);
    let _rt = isolated_runtime_dir();
    let (server, births, _, _) = bind_observed(test, None);
    let _server = server.expect("the server binds");
    let birth = birth_of(&births, role);
    #[cfg(target_os = "linux")]
    assert_born_close_on_exec(birth);
    assert_published_close_on_exec(birth);
    assert_published_nonblocking(birth);
}

#[test]
fn wake_read_is_born_close_on_exec() {
    wake_birth("cloexec::wake_read_is_born_close_on_exec", "wake.read");
}

#[test]
fn wake_write_is_born_close_on_exec() {
    wake_birth("cloexec::wake_write_is_born_close_on_exec", "wake.write");
}

#[test]
fn connector_is_born_close_on_exec() {
    let test = "cloexec::connector_is_born_close_on_exec";
    isolated!(test);
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = io_named!(
        test,
        "server.bind",
        "server bound",
        None,
        SocketServer::bind(&id, 2)
    )
    .unwrap();
    let (client, births, _) = observe_births(None, || {
        io_named!(
            test,
            "client.connect",
            "connected under observation",
            None,
            sot_log::lane::client::Endpoint::connect_voyage_unchallenged(
                &sot_log::lane::socket_unix::SocketEndpoint,
                "voyage",
                &id
            )
        )
    });
    let client = client.expect("the client connects");
    let birth = birth_of(&births, "connector");
    #[cfg(target_os = "linux")]
    assert_born_close_on_exec(birth);
    // The connector's socket became the client's stream: its descriptor is still open and flagged.
    assert_published_close_on_exec(birth);
    drop(client);
    drop(server);
}

/// Every flag-setting call a factory makes fails for real in turn: the factory returns the error and every
/// descriptor it created is closed. Where the platform creates the flags atomically it makes no such call, which
/// only macOS must still prove.
fn flag_failures(test: &str, factory: impl Fn(Option<usize>) -> (bool, Vec<Birth>, usize)) {
    isolated!(test);
    let _rt = isolated_runtime_dir();
    let mut failed = 0;
    for call in 1.. {
        let (made, births, calls) = factory(Some(call));
        if calls < call {
            assert!(made, "no call {call} was made yet the factory failed");
            break;
        }
        assert!(
            !made,
            "flag call {call} failed but the factory returned a descriptor"
        );
        for birth in births {
            assert!(
                closed(birth.fd),
                "{} (fd {}) was left open after flag failure {call}",
                birth.role,
                birth.fd
            );
        }
        failed += 1;
    }
    #[cfg(target_os = "macos")]
    assert!(
        failed > 0,
        "macOS creates these descriptors flagless, so some flag call must exist"
    );
    eprintln!("flag-failure-proof test={test} failed-calls={failed} bodies=1");
}

#[test]
fn a_listener_or_wake_flag_failure_unbinds_and_closes_every_descriptor() {
    let test = "cloexec::a_listener_or_wake_flag_failure_unbinds_and_closes_every_descriptor";
    flag_failures(test, |fail_at| {
        let (result, births, calls, id) = bind_observed(test, fail_at);
        let path = voyage_socket_path(&id).unwrap();
        let made = result.is_ok();
        if !made {
            assert!(!path.exists(), "a failed bind left its socket name behind");
        }
        (made, births, calls)
    });
}

#[test]
fn a_connector_flag_failure_closes_the_socket() {
    let test = "cloexec::a_connector_flag_failure_closes_the_socket";
    let id = fresh_voyage_id();
    flag_failures(test, move |fail_at| {
        let server = SocketServer::bind(&id, 2).expect("the server binds");
        let (result, births, calls) = observe_births(fail_at, || {
            sot_log::lane::client::Endpoint::connect_voyage_unchallenged(
                &sot_log::lane::socket_unix::SocketEndpoint,
                "voyage",
                &id,
            )
        });
        drop(server);
        (result.is_ok(), births, calls)
    });
}
