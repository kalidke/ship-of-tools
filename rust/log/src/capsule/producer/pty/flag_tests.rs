use super::{Producer, PtyProducer};
use std::os::fd::AsRawFd;

fn fd_is_closed(fd: std::os::fd::RawFd) -> bool {
    let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
}

/// The `call`-th flag step (1 the master's, 2 the slave's) fails for real.
fn flag_failure(test: &str, call: usize) {
    if !sot_platform::test_isolated::run_isolated(test) {
        return;
    }
    let argv = ["/bin/sh", "-c", "exit 0"].map(String::from);
    let (spawned, births, calls) = sot_platform::host::fd::observe_births(Some(call), || {
        PtyProducer::spawn(&argv, 80, 24)
    });
    assert!(spawned.is_err(), "PTY flag failure {call} was not returned");
    assert!(calls >= call, "the injected call {call} was never made ({calls})");
    assert_eq!(
        births.iter().map(|b| b.role).collect::<Vec<_>>(),
        ["master", "slave"],
        "the factory owned exactly its two ends: {births:?}"
    );
    for birth in births {
        assert!(
            fd_is_closed(birth.fd),
            "the {} end (fd {}) was left open after the failure",
            birth.role,
            birth.fd
        );
    }
    let child = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(child, -1, "a child was started despite the failure");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    eprintln!("flag-failure-proof test={test} call={call} bodies=1");
}

#[test]
fn master_flag_failure_is_returned() {
    flag_failure("capsule::producer::pty::flag_tests::master_flag_failure_is_returned", 1);
}

#[test]
fn slave_flag_failure_is_returned() {
    flag_failure("capsule::producer::pty::flag_tests::slave_flag_failure_is_returned", 2);
}

/// The pair is close-on-exec in the call that creates it, where the platform can (Linux), not flagged after.
#[cfg(target_os = "linux")]
#[test]
fn a_pty_pair_is_born_close_on_exec() {
    let argv = ["/bin/sh", "-c", "exit 0"].map(String::from);
    let (_producer, births, _) = sot_platform::host::fd::observe_births(None, || {
        PtyProducer::spawn(&argv, 80, 24).expect("spawn")
    });
    assert_eq!(births.len(), 2, "{births:?}");
    for birth in births {
        assert!(
            birth.fd_flags >= 0 && birth.fd_flags & libc::FD_CLOEXEC != 0,
            "the {} end was not close-on-exec at birth: {birth:?}",
            birth.role
        );
    }
}

/// Publication: a successfully spawned producer's master and held slave are close-on-exec.
#[test]
fn a_published_pty_pair_is_close_on_exec() {
    let producer =
        PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();
    for (end, fd) in [
        (
            "master",
            producer.reader_fd.as_ref().expect("the master").as_raw_fd(),
        ),
        (
            "slave",
            producer.slave.as_ref().expect("the held slave").as_raw_fd(),
        ),
    ] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "the {end} end is not close-on-exec at publication"
        );
    }
}
