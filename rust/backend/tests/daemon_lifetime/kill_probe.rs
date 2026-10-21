//! The kill probe: `/bin/kill`, alone in its own group, sends SIGUSR1 to group 0, itself, so how it ends shows how the
//! signal reached it. Three controls spawned by std, each with SIGUSR1's disposition and its mask set here rather than
//! inherited (std keeps the spawning thread's mask), show this host's `/bin/kill` tells the cases apart, so a red names
//! either the host's `/bin/kill` (a control) or the launcher (the launched probe), never one for the other. No shell
//! runs in it: dash, Ubuntu's /bin/sh, clears an inherited mask at startup.

use crate::fixture_owner::Fixture;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

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
/// ignored nor blocked), `survived` (exit 0: it arrived ignored or blocked) or `refused` (any other end: an exit other
/// than 0, as when /bin/kill refuses its arguments, or a death by another signal).
pub fn assert_kill_end(who: &str, status: ExitStatus, stderr: &[u8], want: &str) {
    let end = match (status.signal(), status.code()) {
        (Some(libc::SIGUSR1), _) => "killed",
        (_, Some(0)) => "survived",
        _ => "refused",
    };
    let stderr = String::from_utf8_lossy(stderr);
    assert_eq!(end, want, "{who}: {status}, stderr: {stderr}");
}

/// How a control gets SIGUSR1: at its default, ignored by this process across the spawn, or blocked in its own mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Usr1 {
    Default,
    Ignored,
    Blocked,
}

/// A control: `/bin/kill` spawned by std, alone in its group and with its mask set before the exec (empty, or only
/// SIGUSR1 for `Blocked`), and SIGUSR1's disposition set here across the spawn. Killed at the default; else survived.
pub fn kill_control(fx: &mut Fixture, usr1: Usr1) {
    let mut control = std::process::Command::new("/bin/kill");
    control.args(KILL_PROBE).process_group(0);
    control.stderr(Stdio::piped());
    let block = usr1 == Usr1::Blocked;
    // SAFETY: sigemptyset, sigaddset, sigprocmask, getpgrp and getpid are async-signal-safe; nothing here allocates.
    unsafe {
        control.pre_exec(move || {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            if block {
                libc::sigaddset(&mut mask, libc::SIGUSR1);
            }
            if libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::getpgrp() == libc::getpid() {
                true => Ok(()),
                false => Err(std::io::Error::from_raw_os_error(libc::EPERM)),
            }
        })
    };
    let disposition = match usr1 {
        Usr1::Ignored => libc::SIG_IGN,
        Usr1::Default | Usr1::Blocked => libc::SIG_DFL,
    };
    let child = with_usr1(disposition, || control.spawn())
        .expect("spawn the kill control (EPERM: it was not alone in its group)");
    fx.own(child.id() as i32, "kill control").unwrap();
    let (status, _, stderr) = sot_platform::test_isolated::drain(child).wait_within(BOUND);
    let want = if usr1 == Usr1::Default {
        "killed"
    } else {
        "survived"
    };
    let who = format!("the {usr1:?} control");
    assert_kill_end(&who, status, stderr.as_bytes(), want);
}
