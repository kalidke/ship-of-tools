use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A child that sleeps; it holds `cookie` as its `XAUTHORITY` when one is given. Killed when dropped.
struct Sleeper(Child);

impl Sleeper {
    fn start(cookie: Option<&Path>) -> Self {
        let mut cmd = Command::new("sleep");
        cmd.arg("60").stdin(Stdio::null()).env_remove("XAUTHORITY");
        if let Some(cookie) = cookie {
            cmd.env("XAUTHORITY", cookie);
        }
        let child = cmd.spawn().expect("start sleep");
        // Until its exec, /proc shows the environment of the forking test, not the one it was given.
        let cmdline = format!("/proc/{}/cmdline", child.id());
        while !std::fs::read(&cmdline).is_ok_and(|c| c.starts_with(b"sleep")) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Sleeper(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Whether it ended within a bound.
    fn ends(&mut self) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if self.0.try_wait().expect("wait").is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cookie(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sot-holders-test-{}-{name}", std::process::id()))
}

#[test]
fn a_holder_started_after_the_first_listing_still_ends() {
    let cookie = cookie("repeat");
    let (mut first, mut late) = (None, None);
    let mut calls = 0;
    let signalled = end_with(&cookie, || {
        calls += 1;
        if calls > 1 {
            return processes();
        }
        // The first listing names only `first`; `late` starts after it, as a fork between a walk and the kill does.
        first = Some(Sleeper::start(Some(&cookie)));
        let listed = vec![first.as_ref().unwrap().pid()];
        late = Some(Sleeper::start(Some(&cookie)));
        listed
    });
    assert!(first.as_mut().unwrap().ends(), "the listed holder kept running");
    assert!(late.as_mut().unwrap().ends(), "the holder started after the first listing kept running");
    assert_eq!(signalled, 2);
}

#[test]
fn a_live_process_that_holds_no_cookie_is_not_signalled() {
    let cookie = cookie("reread");
    let wanted = [b"XAUTHORITY=".as_slice(), cookie.as_os_str().as_bytes()].concat();
    // SAFETY: geteuid has no preconditions.
    let account = unsafe { libc::geteuid() };
    let mut other = Sleeper::start(None);
    assert!(end_one(other.pid(), account, &wanted).is_none(), "a process without the cookie was signalled");
    std::thread::sleep(Duration::from_millis(100));
    assert!(other.0.try_wait().expect("wait").is_none(), "a process without the cookie ended");
    let mut holder = Sleeper::start(Some(&cookie));
    assert!(end_one(holder.pid(), account, &wanted).is_some());
    assert!(holder.ends(), "a holder was not ended");
}
