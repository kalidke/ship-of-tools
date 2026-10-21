use super::{Producer, PtyProducer};
use std::io::Read;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
fn proc_state(pid: libc::pid_t) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    stat[close + 1..]
        .split_whitespace()
        .next()
        .map(str::to_string)
}

#[cfg(target_os = "linux")]
fn wait_for_zombie(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if proc_state(pid).as_deref() == Some("Z") {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// "Dead" here means EITHER a zombie (`Z`, awaiting reap by whoever
/// its current parent is) OR fully gone (`/proc` entry absent) --
/// never "gone" alone, which would depend on how fast some external
/// subreaper happens to reap an orphan, not on what this producer's
/// own `Drop` did.
#[cfg(target_os = "linux")]
fn wait_until_dead(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match proc_state(pid) {
            Some(s) if s == "Z" => return true,
            None => return true, // /proc entry gone -- also dead
            Some(_) => {}        // still alive in some other state
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One scan of `/proc` for a LIVE process whose OWN pgrp (field 5)
/// equals `leader_pid` and whose pid is NOT `leader_pid` itself —
/// see this module's own doc for why this replaces reading the
/// leader's own `children` file.
#[cfg(target_os = "linux")]
fn find_descendant_by_pgrp(leader_pid: libc::pid_t) -> Option<libc::pid_t> {
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<libc::pid_t>() else {
            continue;
        };
        if pid == leader_pid {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut fields = stat[close + 1..].split_whitespace();
        let Some(_state) = fields.next() else {
            continue;
        };
        let Some(_ppid) = fields.next() else { continue };
        let Some(pgrp) = fields.next().and_then(|s| s.parse::<libc::pid_t>().ok()) else {
            continue;
        };
        if pgrp == leader_pid {
            return Some(pid);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn wait_for_descendant_by_pgrp(
    leader_pid: libc::pid_t,
    timeout: Duration,
) -> Option<libc::pid_t> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = find_descendant_by_pgrp(leader_pid) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A reader blocked on a
/// `take_output()` `File` must NOT be stranded forever by an early
/// `Drop` (a panicking `run`, or any path that skips
/// `close_output_side`/teardown entirely). `sleep 600` as the
/// producer: silent forever, so the reader's blocking `read` is
/// GENUINELY still parked (not merely lucky timing) when we drop.
/// The leg's child sees the terminal the pty owner declares — read
/// back through the pty itself, not from the producer's own process.
/// Only the bytes are asserted (a reader thread, as the early-Drop
/// test below): the child's exit is not part of the claim.
#[test]
fn the_child_sees_a_256_colour_truecolor_terminal() {
    let argv = ["/bin/sh", "-c", "printf '<%s|%s>' \"$TERM\" \"$COLORTERM\""].map(String::from);
    let mut producer = PtyProducer::spawn(&argv, 80, 24).unwrap();
    let mut output = producer.take_output();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match output.read(&mut buf) {
                Ok(n) if n > 0 => {
                    seen.extend_from_slice(&buf[..n]);
                    if seen.ends_with(b">") {
                        break;
                    }
                }
                _ => break,
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
    });
    let seen = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the pty never delivered the marker");
    assert!(
        seen.contains("<xterm-256color|truecolor>"),
        "pty output was {seen:?}"
    );
    drop(producer);
    reader.join().unwrap();
}

#[test]
fn drop_before_phase_b_does_not_strand_the_reader() {
    let mut producer =
        PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();
    let mut output = producer.take_output();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let _ = tx.send(output.read(&mut buf));
    });

    std::thread::sleep(Duration::from_millis(100));
    assert!(
        rx.try_recv().is_err(),
        "the reader must still be blocked before the early drop (sleep is silent)"
    );

    drop(producer); // no close_output_side/terminate_domain call -- simulates an early return/panic

    let result = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the reader never unblocked after an early Drop");
    // An ordered EOF (`Ok(0)`) or an I/O error (`EIO`) are both an
    // acceptable "unblocked, not stranded" outcome -- Drop's own real
    // behavior (kill + reap the child, drop the held slave and the
    // write-side dup) is what actually severs the last reference;
    // which shape the kernel picks between the two is unspecified,
    // same as the module doc's own EOF/EIO note.
    if let Ok(n) = result {
        assert_eq!(n, 0, "expected an ordered EOF, got {n} bytes");
    }
    reader.join().unwrap();
}

/// After the LEADER has already exited (and this producer has
/// deliberately NOT reaped it — see the module doc's third point), a
/// surviving DESCENDANT in the same process group must still be
/// killed by `Drop` alone, with no `terminate_domain`/
/// `close_output_side` call ever made. `trap '' HUP` on the
/// backgrounded `sleep` (inherited across its own exec, since
/// `SIG_IGN` survives `exec` unlike a caught handler) is what lets it
/// survive the leader's own exit at all — the leader, as a session
/// leader with a controlling tty, would otherwise send it a real
/// `SIGHUP` on exit, and the test would prove nothing about `Drop`
/// specifically (mirrors `tests/e2e_socket/`'s identical finding,
/// F7). No delay between backgrounding and the shell's own `exit 0`
/// is needed: `find_descendant_by_pgrp` finds the descendant by its
/// OWN pgrp, which survives the leader's exit/reparenting untouched.
#[test]
#[cfg(target_os = "linux")]
fn drop_kills_surviving_descendants_when_the_leader_already_exited() {
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "trap '' HUP; sleep 600 & exit 0".to_string(),
    ];
    let producer = PtyProducer::spawn(&argv, 80, 24).unwrap();

    assert!(
        wait_for_zombie(producer.pid, Duration::from_secs(5)),
        "the leader never exited within 5s"
    );
    let descendant_pid = wait_for_descendant_by_pgrp(producer.pid, Duration::from_secs(5))
        .expect(
            "the backgrounded sleep must survive the leader's own exit (SIGHUP is ignored)",
        );

    drop(producer); // no terminate_domain/close_output_side call -- Drop alone must do this

    assert!(
        wait_until_dead(descendant_pid, Duration::from_secs(5)),
        "the surviving descendant was still alive 5s after Drop"
    );
}

/// The UNREAPED ZOMBIE LEADER is the deterministic
/// case of the identical property this producer's own `domain_is_empty`
/// must get right: a live leader means "not empty"; the SAME leader,
/// killed and left an unreaped zombie (this producer's own contract
/// -- see the module doc's third point), must read as "empty".
#[test]
#[cfg(target_os = "linux")]
fn domain_is_empty_ignores_a_zombie_leader() {
    let producer =
        PtyProducer::spawn(&["sleep".to_string(), "600".to_string()], 80, 24).unwrap();

    assert!(
        !producer.domain_is_empty().unwrap(),
        "a live leader means the domain is not empty"
    );

    producer.terminate_domain().unwrap();
    assert!(
        wait_for_zombie(producer.pid, Duration::from_secs(5)),
        "the leader never became a zombie after terminate_domain within 5s"
    );

    assert!(
        producer.domain_is_empty().unwrap(),
        "a domain containing only an unreaped zombie leader must read as empty"
    );
}
