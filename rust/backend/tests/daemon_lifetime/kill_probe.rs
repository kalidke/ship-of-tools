//! The kill probe: `/bin/kill`, alone in its own group, sends SIGUSR1 to group 0, itself, so how it ends shows how the
//! signal reached it. Two controls spawned by std show this host's `/bin/kill` tells the cases apart, so a red names
//! either the host's `/bin/kill` (a control) or the launcher (the launched probe), never one for the other. No shell
//! runs in it: dash, Ubuntu's /bin/sh, clears an inherited mask at startup.

use crate::fixture_owner::Fixture;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ExitStatus, Stdio};

/// `/bin/kill`'s arguments: SIGUSR1 to group 0, which is its own group when it is alone in it.
pub const KILL_PROBE: [&str; 3] = ["-s", "USR1", "0"];

/// `f` with SIGUSR1's disposition `usr1` in this process: what a child forked meanwhile inherits.
pub fn with_usr1<T>(usr1: libc::sighandler_t, f: impl FnOnce() -> T) -> T {
    // SAFETY: restored below; SIGUSR1 is otherwise unused by this test binary.
    let previous = unsafe { libc::signal(libc::SIGUSR1, usr1) };
    let result = f();
    // SAFETY: puts back what was there.
    unsafe { libc::signal(libc::SIGUSR1, previous) };
    result
}

/// How `/bin/kill` with `KILL_PROBE`, alone in its group, ended must be `want`: `killed` (SIGUSR1 arrived neither
/// ignored nor blocked), `survived` (exit 0: it arrived ignored or blocked) or `refused` (any other end: /bin/kill
/// refused its arguments).
pub fn assert_kill_end(who: &str, status: ExitStatus, stderr: &[u8], want: &str) {
    let end = match (status.signal(), status.code()) {
        (Some(libc::SIGUSR1), _) => "killed",
        (_, Some(0)) => "survived",
        _ => "refused",
    };
    let stderr = String::from_utf8_lossy(stderr);
    assert_eq!(end, want, "{who}: {status}, stderr: {stderr}");
}

/// A control: `/bin/kill` spawned by std, which clears the mask but keeps an ignored SIGUSR1, alone in its group
/// (refused before the exec otherwise), with SIGUSR1 at `usr1` in this process across the spawn.
pub fn kill_control(fx: &mut Fixture, usr1: libc::sighandler_t, want: &str) {
    let mut control = std::process::Command::new("/bin/kill");
    control.args(KILL_PROBE).process_group(0);
    control.stderr(Stdio::piped());
    // SAFETY: getpgrp and getpid are async-signal-safe, and the refusal allocates nothing.
    unsafe {
        control.pre_exec(|| match libc::getpgrp() == libc::getpid() {
            true => Ok(()),
            false => Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        })
    };
    let child = with_usr1(usr1, || control.spawn()).unwrap();
    fx.own(child.id() as i32, "kill control").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_kill_end("the control", out.status, &out.stderr, want);
}
