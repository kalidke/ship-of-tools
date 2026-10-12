//! The macOS half of the start/fire tests: finished-group recognition before reap on real processes,
//! rejected-observation error evidence with injected faults, and a real-request Result control of fixture diagnostics.

use super::unix::Watched;
use crate::lifecycle::child_signal::{ContainedStd, Signal};
use crate::lifecycle::contain;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Wait until the fixture process has written its readiness file.
fn wait_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(path).map_or(true, |s| s.is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture readiness did not arrive: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn zombie_command() -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("finished");
    sot_log::test_exec::write_executable(&program, "#!/bin/sh\nexit 17\n");
    (dir, Command::new(program))
}
fn exited_eperm(pid: u32) {
    check_exited_eperm(pid).expect("real EPERM fixture");
}
fn check_exited_eperm(pid: u32) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !contain::exited_pid(pid, false)
        .map_err(|error| format!("observe retained owned leader: {error}"))?
    {
        if std::time::Instant::now() >= deadline {
            return Err("leader must remain exited-unreaped within 15 s".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = contain::macos::observe(pid as i32);
    let (uid, ruid) = unsafe { (libc::geteuid(), libc::getuid()) };
    // SAFETY: this test still exclusively owns the unreaped leader and its group.
    let rc = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    let error = (rc != 0).then(std::io::Error::last_os_error);
    let after = contain::macos::observe(pid as i32);
    let errno = error
        .as_ref()
        .and_then(std::io::Error::raw_os_error)
        .map_or_else(|| "none".to_string(), |errno| errno.to_string());
    let evidence = format!("rc={rc} errno={errno} caller uid={uid} ruid={ruid} group={pid}; before: {before}; after: {after}");
    if rc != -1 || error.as_ref().and_then(std::io::Error::raw_os_error) != Some(libc::EPERM) {
        return Err(format!(
            "fixture must demonstrate real group EPERM: {evidence}"
        ));
    }
    eprintln!("real group request: EPERM (errno=1); retained leader exited-unreaped");
    Ok(())
}
fn clear_events() {
    contain::REQUEST_EVENTS.with(|events| events.borrow_mut().clear());
    contain::macos::EVENTS.with(|events| events.borrow_mut().clear());
}
fn accepted_observations() {
    let events = contain::macos::EVENTS.with(|events| events.borrow().clone());
    eprintln!("no-live observations: {events:?}");
    assert_eq!(
        events,
        [
            "exited-unreaped",
            "members-any-uid",
            "status",
            "members-any-uid",
            "status",
            "exited-unreaped"
        ],
        "zombie-only acceptance bypassed complete membership/status observation"
    );
    assert_eq!(
        contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
        ["group", "leader"]
    );
}
#[test]
fn zombie_only_group_returns_the_std_status() {
    let (_dir, mut command) = zombie_command();
    let mut child = Box::leak(Box::new(Signal::new()))
        .spawn_std(&mut command)
        .unwrap();
    exited_eperm(child.id());
    clear_events();
    let result = child.wait_within(Duration::from_secs(3));
    let status = result
        .expect("zombie-only std group must return original status")
        .unwrap();
    assert!(
        child.confirmed_reaped() && status.code() == Some(17),
        "original std status and confirmed reap"
    );
    accepted_observations();
}
#[tokio::test(flavor = "current_thread")]
async fn zombie_only_group_returns_the_async_status() {
    let (_dir, command) = zombie_command();
    let signal = Box::leak(Box::new(Signal::new()));
    let mut child = signal.spawn(&mut command.into()).unwrap();
    exited_eperm(signal.held_groups()[0] as u32);
    clear_events();
    let status = child
        .wait()
        .await
        .expect("zombie-only async group must return original status");
    assert_eq!(status.code(), Some(17));
    assert_eq!(child.wait().await.unwrap().code(), Some(17));
    accepted_observations();
}
struct LiveTree {
    dir: tempfile::TempDir,
    child: Option<ContainedStd>,
    descendant: Option<Watched>,
}
impl LiveTree {
    fn new(root: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (leader, member) = (dir.path().join("leader"), dir.path().join("member"));
        // The member keeps inherited stdin, exits on EOF, and has an independent 30 s ceiling.
        sot_log::test_exec::write_executable(&member, "#!/usr/bin/env python3\nimport os,sys,time,select\nos.setpgid(0,int(sys.argv[1]))\nwith open(sys.argv[2],'w') as f: f.write('%d %d %d'%(os.getpid(),os.getpgrp(),os.geteuid()))\nend=time.monotonic()+30\nwhile time.monotonic()<end:\n if select.select([0],[],[],max(0,end-time.monotonic()))[0]:\n  data=os.read(0,4096)\n  if not data: break\n  with open(sys.argv[2]+'.live','w') as f: f.write('alive')\n");
        sot_log::test_exec::write_executable(&leader, "#!/usr/bin/env python3\nimport os,sys,time,subprocess\nmember,ready,release,cleanup,root=sys.argv[1:]\nargs=[sys.executable,member,str(os.getpid()),ready]\np=subprocess.Popen((['/usr/bin/sudo','-n','--'] if root=='root' else [])+args)\nend=time.monotonic()+20\nwhile not os.path.exists(release) and time.monotonic()<end:\n if p.poll() is not None: raise RuntimeError('owned descendant exited before readiness/release; sudo -n is required')\n time.sleep(.01)\nif os.path.exists(cleanup): p.wait(timeout=35)\nsys.exit(17)\n");
        let signal = Box::leak(Box::new(Signal::new()));
        let mut command = Command::new(leader);
        let paths = ["ready", "release", "cleanup"].map(|name| dir.path().join(name));
        command
            .arg(member)
            .args(paths)
            .arg(if root { "root" } else { "same" })
            .stdin(Stdio::piped());
        let child = signal.spawn_std(&mut command).unwrap();
        let mut tree = Self {
            dir,
            child: Some(child),
            descendant: None,
        };
        wait_file(&tree.dir.path().join("ready"));
        let text = std::fs::read_to_string(tree.dir.path().join("ready")).unwrap();
        let ids: Vec<u32> = text
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        tree.descendant = Some(Watched::open(ids[0]));
        assert_eq!(
            ids[1],
            tree.child.as_ref().unwrap().id(),
            "STOP: sudo changed the retained group"
        );
        assert_eq!(
            unsafe { libc::getpgid(ids[0] as i32) },
            ids[1] as i32,
            "observed live member group"
        );
        assert_eq!(ids[2], if root { 0 } else { unsafe { libc::geteuid() } });
        assert!(
            !root || unsafe { libc::geteuid() } != 0,
            "genuine denial requires different credentials"
        );
        eprintln!("owned live member pgid matches retained leader; cross-uid={root}; stdin-close lifetime armed");
        std::fs::write(tree.dir.path().join("release"), b"exit").unwrap();
        assert!(contain::exited_pid(tree.child.as_ref().unwrap().id(), true).unwrap());
        tree
    }
}
impl Drop for LiveTree {
    fn drop(&mut self) {
        let mut child = self.child.take().unwrap();
        drop(child.stdin.take());
        std::fs::write(self.dir.path().join("cleanup"), b"close").unwrap();
        std::fs::write(self.dir.path().join("release"), b"exit").unwrap();
        if let Some(watched) = self.descendant.take() {
            let began = std::time::Instant::now();
            while !watched.dead() {
                if began.elapsed() > Duration::from_secs(35) {
                    std::mem::forget(child); // Never let product Drop signal the root fixture on a failed EOF cleanup.
                    panic!("owned member did not exit within the stdin-close deadline");
                }
            }
            eprintln!("owned descendant exit observed after stdin close");
        }
        let began = std::time::Instant::now();
        while let Err(error) = child.wait_within(Duration::from_secs(40)) {
            eprintln!("owned fixture cleanup retry before reap: {error}");
            if began.elapsed() > Duration::from_secs(5) {
                std::mem::forget(child);
                panic!("owned fixture cleanup did not reap before its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
#[test]
fn an_exited_leaders_live_descendant_still_receives_the_group_signal() {
    let mut tree = LiveTree::new(false);
    let pid = tree.child.as_ref().unwrap().id();
    assert!(
        contain::macos::checked_no_live_group(pid as i32).is_err(),
        "live member accepted as absence"
    );
    clear_events();
    assert_eq!(
        tree.child.as_mut().unwrap().wait().unwrap().code(),
        Some(17)
    );
    assert_eq!(
        contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
        ["group", "leader"]
    );
    assert!(
        tree.descendant.take().unwrap().dead(),
        "real group request did not end the owned descendant"
    );
}
#[test]
fn a_real_live_group_permission_denial_stays_an_error() {
    use std::io::Write;
    let mut tree = LiveTree::new(true);
    exited_eperm(tree.child.as_ref().unwrap().id());
    let child = tree.child.as_mut().unwrap();
    child.stdin.as_mut().unwrap().write_all(b"probe").unwrap();
    wait_file(&tree.dir.path().join("ready.live"));
    eprintln!("owned root member still live after genuine group EPERM");
    clear_events();
    let result = child.wait();
    assert_eq!(
        contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
        ["group", "leader"]
    );
    let error = result.expect_err("genuine live-member EPERM was accepted as absence");
    assert!(error.to_string().contains("macOS group EPERM:"), "{error}");
    assert!(error.to_string().contains("step=member-first"), "{error}");
    assert_original_eperm(&error);
}
#[test]
fn a_failed_group_observation_keeps_the_original_eperm() {
    for fault in 1..=12 {
        let (_dir, mut command) = zombie_command();
        let mut child = Box::leak(Box::new(Signal::new()))
            .spawn_std(&mut command)
            .unwrap();
        exited_eperm(child.id());
        clear_events();
        contain::macos::FAULT.with(|value| value.set(fault));
        let result = child.wait();
        contain::macos::FAULT.with(|value| value.set(0));
        let error = result.expect_err("failed or ambiguous zombie-group observation was accepted");
        assert_fault_evidence(&error, fault);
        assert_original_eperm(&error);
        child.wait().unwrap();
        let requests = contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
        assert_eq!(requests, ["group", "leader", "group", "leader"]);
        eprintln!(
            "query fault={fault}: original EPERM preserved; independent leader request checked"
        );
    }
    println!(
        "MAC-EPERM evidence PASS: 12 rejected observations retain request errno and step evidence"
    );
}
fn assert_original_eperm(error: &std::io::Error) {
    let source = error
        .get_ref()
        .and_then(|inner| inner.source())
        .and_then(|source| source.downcast_ref::<std::io::Error>());
    assert!(
        error.kind() == std::io::ErrorKind::PermissionDenied
            && error.raw_os_error().is_none()
            && source.and_then(std::io::Error::raw_os_error) == Some(libc::EPERM),
        "group EPERM must keep PermissionDenied, no outer errno and the original EPERM as its source: kind={:?} {error}",
        error.kind()
    );
}
fn assert_fault_evidence(error: &std::io::Error, fault: u8) {
    let text = error.to_string();
    let stage = match fault {
        1 | 2 | 5 | 9 => "membership-first",
        8 => "membership-second",
        12 => "member-compare",
        _ => "member-first",
    };
    for required in [
        "macOS group EPERM:",
        &format!("step={stage}"),
        "group=",
        "errno=1",
    ] {
        assert!(
            text.contains(required),
            "fault={fault} missing {required}: {text}"
        );
    }
    if fault == 3 {
        assert!(
            text.contains("step=member-first reason=query-failed"),
            "fault=3 missing reason=query-failed: {text}"
        );
    }
    if fault == 4 {
        assert!(
            text.contains("reason=short-record"),
            "fault=4 missing reason=short-record: {text}"
        );
    }
    let queries = contain::macos::EVENTS.with(|events| {
        events
            .borrow()
            .iter()
            .filter(|event| **event == "status")
            .count()
    });
    let records =
        text.matches("member-first pid=").count() + text.matches("member-second pid=").count();
    assert_eq!(
        records, queries,
        "one stage record per captured BSD query: {text}"
    );
    if fault == 1 || fault == 9 {
        assert!(
            text.contains("injected-count="),
            "fault={fault} missing injected count: {text}"
        );
    }
    match fault {
        3 | 4 => {
            for required in ["status=unknown uid=unknown", "count=", "expected="] {
                assert!(text.contains(required), "fault={fault}: {text}");
            }
        }
        6 | 7 | 10 | 11 => {
            for required in [
                "pid=",
                "returned-pid=",
                "group=",
                "status=",
                "uid=",
                "ruid=",
            ] {
                assert!(text.contains(required), "fault={fault}: {text}");
            }
        }
        8 => {
            assert!(text.contains("membership-first sorted=["), "{text}");
            assert!(text.contains("membership-second sorted=[]"), "{text}");
        }
        12 => {
            assert!(text.contains("member-first pid="), "{text}");
            assert!(text.contains("member-second pid="), "{text}");
            assert!(text.contains("creation-time-changed"), "{text}");
            assert!(text.matches("created=").count() >= 2, "{text}");
        }
        _ => {}
    }
    assert!(
        format!("{error:?}").contains("step="),
        "Debug lost observation: {error:?}"
    );
}
#[test]
fn a_non_eperm_fixture_failure_reports_its_observation() {
    let tree = LiveTree::new(false);
    let pid = tree.child.as_ref().unwrap().id();
    let text =
        check_exited_eperm(pid).expect_err("same-credential group must fail the EPERM premise");
    for required in [
        "fixture must demonstrate real group EPERM",
        "rc=0",
        "errno=none",
        "caller uid=",
        "ruid=",
        "group=",
        "before:",
        "after:",
        "pid=",
        "status=",
        "uid=",
    ] {
        assert!(text.contains(required), "missing {required}: {text}");
    }
    println!("MAC-EPERM fixture evidence PASS: real non-EPERM request names rc, credentials, before and after");
}
#[test]
fn injected_group_and_leader_failures_stay_checked() {
    for failure in 1..=3 {
        let (_dir, mut command) = zombie_command();
        let mut child = Box::leak(Box::new(Signal::new()))
            .spawn_std(&mut command)
            .unwrap();
        exited_eperm(child.id());
        clear_events();
        contain::REQUEST_FAILURE.with(|value| value.set(failure));
        let result = child.wait();
        contain::REQUEST_FAILURE.with(|value| value.set(0));
        accepted_observations();
        child.wait().unwrap();
        let error = result
            .expect_err("recognized zombie group suppressed injected request failure")
            .to_string();
        assert!(
            failure & 1 == 0 || error.contains("group request failure"),
            "{error}"
        );
        assert!(
            failure & 2 == 0 || error.contains("leader request failure"),
            "{error}"
        );
    }
}
