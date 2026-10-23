//! Descriptor flags of the connector and of the row listener, observed on the real descriptors: close-on-exec at birth
//! where the platform creates it atomically (Linux), at publication everywhere, and a failed flag call returned with
//! every descriptor closed. The window between creation and flag on macOS is closed by `host::fd`'s birth lock, which
//! its own tests and the hosted macOS run cover.

use super::*;
use sot_log::lane::connect_own::connect_own;
use sot_platform::host::fd::{observe_births, Birth};

fn birth_of<'a>(births: &'a [Birth], role: &str) -> &'a Birth {
    births
        .iter()
        .find(|b| b.role == role)
        .unwrap_or_else(|| panic!("the factory never created a {role} descriptor: {births:?}"))
}

#[cfg(target_os = "linux")]
fn assert_born_close_on_exec(birth: &Birth) {
    assert!(birth.fd_flags >= 0 && birth.fd_flags & libc::FD_CLOEXEC != 0, "descriptor missing FD_CLOEXEC at birth: {birth:?}");
}

fn assert_published_close_on_exec(birth: &Birth) {
    let now = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
    assert!(now >= 0 && now & libc::FD_CLOEXEC != 0, "{} was published without FD_CLOEXEC", birth.role);
}

fn closed(fd: std::os::fd::RawFd) -> bool {
    let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
}

/// The row's listening socket is close-on-exec: no child this process starts can hold it. The listener's descriptor is
/// found by its inode, which `/proc/net/unix` pairs with the row's path.
#[test]
#[cfg(target_os = "linux")]
fn listener_is_born_close_on_exec() {
    if !run_isolated("cloexec::listener_is_born_close_on_exec") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let table = std::fs::read_to_string("/proc/net/unix").expect("the kernel's socket table");
    let want = path.to_str().unwrap();
    let inode: u64 = table
        .lines()
        .filter(|line| line.ends_with(want))
        .map(|line| line.split_whitespace().nth(6).expect("an inode column").parse().expect("a number"))
        .next()
        .unwrap_or_else(|| panic!("no socket is bound at {want}"));
    let target = format!("socket:[{inode}]");
    let mut found = 0;
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if std::fs::read_link(entry.path()).is_ok_and(|link| link.to_str() == Some(target.as_str())) {
            let fd: std::os::fd::RawFd = entry.file_name().to_str().unwrap().parse().unwrap();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "the row's listener (fd {fd}) is not close-on-exec");
            found += 1;
        }
    }
    assert!(found > 0, "the listener's descriptor is not open in this process");
    server.shutdown();
}

#[test]
fn connector_is_born_close_on_exec() {
    if !run_isolated("cloexec::connector_is_born_close_on_exec") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let (client, births, _) = observe_births(None, || connect_own(&path));
    let client = client.expect("the client connects");
    let birth = birth_of(&births, "connector");
    #[cfg(target_os = "linux")]
    assert_born_close_on_exec(birth);
    // The connector's socket became the client's stream: its descriptor is still open and flagged.
    assert_published_close_on_exec(birth);
    drop(client);
    server.shutdown();
}

/// Every flag-setting call the connector makes fails for real in turn: it returns the error and every descriptor it
/// created is closed. Where the platform creates the flags atomically it makes no such call, which only macOS must
/// still prove.
#[test]
fn a_connector_flag_failure_closes_the_socket() {
    if !run_isolated("cloexec::a_connector_flag_failure_closes_the_socket") {
        return;
    }
    let (_dir, path, server) = bind_row();
    let mut failed = 0;
    for call in 1.. {
        let (result, births, calls) = observe_births(Some(call), || connect_own(&path));
        if calls < call {
            assert!(result.is_ok(), "no call {call} was made yet the connector failed");
            break;
        }
        assert!(result.is_err(), "flag call {call} failed but the connector returned a descriptor");
        for birth in births {
            assert!(closed(birth.fd), "{} (fd {}) was left open after flag failure {call}", birth.role, birth.fd);
        }
        failed += 1;
    }
    #[cfg(target_os = "macos")]
    assert!(failed > 0, "macOS creates these descriptors flagless, so some flag call must exist");
    eprintln!("flag-failure-proof test=a_connector_flag_failure_closes_the_socket failed-calls={failed} bodies=1");
    server.shutdown();
}
