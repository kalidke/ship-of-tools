use super::*;

#[test]
fn discover_or_mint_voyage_start_mints_a_fresh_voyage_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == id));
    assert!(voyage_root_path(dir.path(), &id).exists());
}

#[test]
fn discover_or_mint_voyage_resume_refuses_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
}

#[test]
fn discover_or_mint_voyage_returns_the_existing_id_when_valid() {
    let dir = tempfile::tempdir().unwrap();
    let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    let again = discover_or_mint_voyage(dir.path(), StartMode::Resume).unwrap();
    assert_eq!(id, again);
}

#[test]
fn discover_or_mint_voyage_refuses_a_corrupt_pointer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(pointer::pointer_path(dir.path()), b"not-a-uuid").unwrap();
    assert!(discover_or_mint_voyage(dir.path(), StartMode::Start).is_err());
    assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
}

#[test]
fn should_spawn_after_absent_start_mode_always_spawns() {
    let dir = tempfile::tempdir().unwrap();
    let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Start).unwrap());
}

#[test]
fn should_spawn_after_absent_resume_with_no_leg_spawns() {
    let dir = tempfile::tempdir().unwrap();
    let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Resume).unwrap());
}

/// A lease end is close-on-exec at publication on every Unix and born so where `pipe2` exists.
#[cfg(unix)]
fn lease_end_is_close_on_exec(role: &str) {
    use sot_platform::host::fd::observe_births;
    let (lease, births, _) = observe_births(None, || LegLease::create("h").expect("the lease"));
    let birth = births
        .iter()
        .find(|b| b.role == role)
        .unwrap_or_else(|| panic!("no {role} descriptor: {births:?}"));
    #[cfg(target_os = "linux")]
    assert!(
        birth.fd_flags >= 0 && birth.fd_flags & libc::FD_CLOEXEC != 0,
        "descriptor missing FD_CLOEXEC at birth: {birth:?}"
    );
    let now = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
    assert!(
        now >= 0 && now & libc::FD_CLOEXEC != 0,
        "{role} published without FD_CLOEXEC"
    );
    drop(lease);
}

#[cfg(unix)]
#[test]
fn lease_read_is_close_on_exec_at_birth_and_publication() {
    lease_end_is_close_on_exec("lease.read");
}

#[cfg(unix)]
#[test]
fn lease_write_is_close_on_exec_at_birth_and_publication() {
    lease_end_is_close_on_exec("lease.write");
}

/// Each flag step of the lease's birth fails for real through `create`, which then closes both ends.
#[cfg(unix)]
#[test]
fn a_lease_flag_failure_closes_both_ends() {
    if !sot_platform::test_isolated::run_isolated(
        "supervisor::leg::tests::a_lease_flag_failure_closes_both_ends",
    ) {
        return;
    }
    use sot_platform::host::fd::observe_births;
    for call in 1..=2 {
        let (made, births, calls) = observe_births(Some(call), || LegLease::create("h"));
        assert!(made.is_err(), "lease flag failure {call} was not returned");
        assert_eq!(births.len(), 2);
        assert!(
            calls >= call,
            "the injected call {call} was never made ({calls})"
        );
        for birth in births {
            let rc = unsafe { libc::fcntl(birth.fd, libc::F_GETFD) };
            assert!(
                rc < 0,
                "{} (fd {}) was left open after failure {call}",
                birth.role,
                birth.fd
            );
        }
    }
    let (made, _, calls) = observe_births(Some(3), || LegLease::create("h"));
    assert!(
        made.is_ok() && calls == 2,
        "the birth makes two flag calls, made {calls}"
    );
    eprintln!("lease-flag-proof calls=2 failures=2 bodies=1");
}

/// A leg's child holds the lease at `PARENT_LEASE_FD` and nothing else past its exec, whatever flag a descriptor
/// had when the leg started. On macOS a socket is made close-on-exec by a second call after it is created; this
/// pipe is caught between the two.
#[cfg(unix)]
#[test]
fn a_leg_keeps_only_its_stdio_and_the_lease_past_its_exec() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let lease = LegLease::create("h").expect("the lease");
    let spawn = lease.for_spawn().expect("a spawn lease");
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (held, other) = (fds[0], fds[1]);
    // SAFETY: clears the flag on this test's own descriptors: the state between macOS's pipe() and its fcntl.
    for fd in [held, other] {
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    }
    let read_fd = spawn.0.as_raw_fd();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(format!(
        "for fd in {held} {other}; do if test -e /dev/fd/$fd; then echo held $fd; fi; done; \
             if test -e /dev/fd/{PARENT_LEASE_FD}; then echo lease; fi"
    ));
    unsafe { cmd.pre_exec(move || install_lease(read_fd)) };
    let ran = sot_platform::test_wait::run_within(&mut cmd, std::time::Duration::from_secs(10)).ok();
    unsafe {
        libc::close(held);
        libc::close(other);
    }
    assert_eq!(ran.stdout.trim(), "lease", "the leg kept more than the lease: {}", ran.stdout);
}
