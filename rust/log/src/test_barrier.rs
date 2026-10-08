//! test_barrier.rs — a named phase barrier a real process can be held at, for the daemon-lifetime harness
//! (rust/backend/tests/daemon_lifetime). Built only with the `native-barrier` feature, which no installed build
//! enables, and inert unless `SOT_TEST_BARRIER_DIR` names a folder.
//!
//! [`hold`] takes the next arrival ticket for the phase. Tickets below the phase's hold count (the number in
//! `<phase>.hold`, default none) are held: the process writes `<phase>.<ticket>.reached` with its own identity and
//! waits for `<phase>.<ticket>.go`. The harness reads the identity, acquires its own authority over the process,
//! and only then writes the go file. A barrier nobody opens ends its process after [`GIVE_UP`], so a failed
//! fixture leaves no immortal process behind.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The most a held process waits for its go file.
pub const GIVE_UP: Duration = Duration::from_secs(300);
/// The status a process that gave up exits with.
pub const GAVE_UP_STATUS: i32 = 97;

const DIR_VAR: &str = "SOT_TEST_BARRIER_DIR";

/// Take the next ticket for `phase`, and wait for its go file if the ticket is held.
pub fn hold(phase: &str) {
    let Some(dir) = std::env::var_os(DIR_VAR).map(PathBuf::from) else {
        return;
    };
    let held_arrivals: usize = std::fs::read_to_string(dir.join(format!("{phase}.hold")))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0);
    let Some(ticket) = take_ticket(&dir, phase) else {
        return;
    };
    let reached = dir.join(format!("{phase}.{ticket}.reached"));
    let tmp = dir.join(format!("{phase}.{ticket}.reached.tmp"));
    if std::fs::write(&tmp, identity_text())
        .and_then(|()| std::fs::rename(&tmp, &reached))
        .is_err()
    {
        return;
    }
    if ticket >= held_arrivals {
        return;
    }
    let go = dir.join(format!("{phase}.{ticket}.go"));
    let began = Instant::now();
    while !go.exists() {
        if began.elapsed() >= GIVE_UP {
            // SAFETY: an immediate exit of a process nobody released; it holds nothing worth unwinding.
            unsafe { libc::_exit(GAVE_UP_STATUS) };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The lowest ticket nobody has taken, claimed by an exclusive create.
fn take_ticket(dir: &Path, phase: &str) -> Option<usize> {
    for ticket in 0.. {
        let claim = dir.join(format!("{phase}.{ticket}.ticket"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim)
        {
            Ok(_) => return Some(ticket),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// `key value` lines naming this process: pid, creation identity, parent, group and session.
fn identity_text() -> String {
    // SAFETY: plain reads of this process's own ids.
    let (pid, ppid, pgid, sid) = unsafe {
        (
            libc::getpid(),
            libc::getppid(),
            libc::getpgrp(),
            libc::getsid(0),
        )
    };
    format!(
        "pid {pid}\ncreated {}\nppid {ppid}\npgid {pgid}\nsid {sid}\n",
        created()
    )
}

#[cfg(target_os = "linux")]
fn created() -> u64 {
    crate::identity::challenge_unix::self_start_ticks().unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn created() -> u64 {
    crate::identity::challenge_macos::self_pidversion()
        .map(u64::from)
        .unwrap_or(0)
}
