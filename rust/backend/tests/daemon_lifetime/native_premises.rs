//! The native premises of the lane, each run on the real launcher and the real fence: a gated child is returned to
//! its owner before it executes; its setup and exec errors are named; the claim a birth carries stays contended in
//! the child's copy through the parent's close, the exec and the parent's death; a child's source group is its
//! parent's. Every process is a real `sh` or `sleep`; every identity is opened while alive and every death is read
//! from it. Unix only: the launcher is the Unix half of `host::process_tree`.

use crate::fixture_owner::Fixture;
use crate::observations::wait_for;
use sot_log::host::process_tree::{Birth, BirthError, Group, Launch, Stage};
use sot_log::supervisor::birth_claim::BirthClaim;
#[cfg(feature = "daemon-lifetime-faults")]
use std::io::Write as _;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);

fn sh(script: &str) -> Launch {
    let mut launch = Launch::new("/bin/sh");
    launch.args(["-c", script]);
    launch
}

fn error_of(err: &std::io::Error) -> &BirthError {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<BirthError>())
        .unwrap_or_else(|| panic!("not a BirthError: {err}"))
}

/// Begin `launch` and take authority over the gated child before anything else happens to it.
fn gated(fx: &mut Fixture, launch: Launch, label: &str) -> (Birth, usize) {
    let birth = launch.begin().expect("begin returns the owned child");
    let index = fx
        .own(birth.pid(), label)
        .expect("authority over the gated child");
    (birth, index)
}

/// `bound` of quiet: the gated child is alive and has not run its target.
fn stays_gated(fx: &Fixture, birth: &Birth, index: usize, mark: &Path) {
    std::thread::sleep(QUIET);
    assert!(!mark.exists(), "the target ran at a closed gate");
    assert!(
        !fx.identity(index).exited(Duration::ZERO),
        "the gated child ended by itself"
    );
    assert!(
        !birth.exited(false).unwrap(),
        "the gated child is not alive by the parent's own wait"
    );
}

#[test]
fn begin_returns_the_owned_child_before_the_target_runs_and_release_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("mark");
    let mut fx = Fixture::new("begin_returns_owned");
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark);
    // This returns at all only because `begin` does not wait for the exec: the gate is closed.
    let (mut birth, index) = gated(&mut fx, launch, "gated target");
    let ready = birth.ready(BOUND).expect("the child reports it is set up");
    assert_eq!(ready.pid, birth.pid());
    stays_gated(&fx, &birth, index, &mark);

    birth.release().expect("GO");
    birth.exec_result(BOUND).expect("the exec happened");
    let status = birth.wait().expect("reap");
    assert!(status.success(), "{status}");
    assert_eq!(
        std::fs::read_to_string(&mark).unwrap().trim(),
        "entered",
        "the target ran after the release"
    );
    assert!(fx.identity(index).exited(BOUND));
    assert!(fx.cleanup().complete());
}

#[test]
fn cancel_ends_the_child_before_the_target_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("mark");
    let mut fx = Fixture::new("cancel_before_exec");
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark);
    let (mut birth, index) = gated(&mut fx, launch, "cancelled child");
    birth.ready(BOUND).unwrap();
    birth.cancel().expect("CANCEL");
    assert!(
        fx.identity(index).exited(BOUND),
        "the cancelled child is still alive"
    );
    let status = birth.wait().expect("reap");
    assert_eq!(
        status.code(),
        Some(125),
        "a cancelled child exits 125, got {status}"
    );
    assert!(fx.identity(index).exited(BOUND));
    assert!(!mark.exists(), "the target ran after a cancel");
    assert!(fx.cleanup().complete());
}

#[test]
fn dropping_a_birth_ends_a_gated_child_and_a_running_one() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("mark");
    let mut fx = Fixture::new("drop_ends_the_child");
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark);
    let (mut gated_birth, gated_index) = gated(&mut fx, launch, "dropped while gated");
    gated_birth.ready(BOUND).unwrap();
    drop(gated_birth);
    assert!(
        fx.identity(gated_index).exited(BOUND),
        "a dropped gated child is still alive"
    );
    assert!(
        !mark.exists(),
        "the target ran for a birth dropped at its gate"
    );

    let (mut running, running_index) =
        gated(&mut fx, sh("exec sleep 600"), "dropped while running");
    running.ready(BOUND).unwrap();
    running.release().unwrap();
    running.exec_result(BOUND).unwrap();
    assert!(!fx
        .identity(running_index)
        .exited(Duration::from_millis(100)));
    drop(running);
    assert!(
        fx.identity(running_index).exited(BOUND),
        "a dropped running child is still alive"
    );
    assert!(fx.cleanup().complete());
}

#[test]
fn setup_and_exec_errors_are_named_by_stage_and_errno_and_the_target_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("mark");
    let mut fx = Fixture::new("setup_and_exec_errors");

    // A working directory that is a file: fchdir fails in the child.
    let file = std::fs::File::create(dir.path().join("not-a-dir")).unwrap();
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark).cwd(OwnedFd::from(file));
    let (mut birth, index) = gated(&mut fx, launch, "bad cwd");
    let err = birth.ready(BOUND).expect_err("the cwd step fails");
    assert_eq!(
        (error_of(&err).stage, error_of(&err).errno),
        (Stage::Cwd, libc::ENOTDIR),
        "{err}"
    );
    assert_eq!(birth.wait().unwrap().code(), Some(126));
    assert!(fx.identity(index).exited(BOUND));

    // A group that does not exist.
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark).group(Group::Join(i32::MAX - 1));
    let (mut birth, _) = gated(&mut fx, launch, "bad group");
    let err = birth.ready(BOUND).expect_err("the group step fails");
    assert_eq!(error_of(&err).stage, Stage::Group, "{err}");
    assert_eq!(birth.wait().unwrap().code(), Some(126));

    // A target that cannot be executed: the child is ready, and the error comes after the release.
    let launch = Launch::new(dir.path().join("no-such-program"));
    let (mut birth, _) = gated(&mut fx, launch, "bad exec");
    birth.ready(BOUND).expect("set up fine");
    birth.release().unwrap();
    let err = birth.exec_result(BOUND).expect_err("the exec step fails");
    assert_eq!(
        (error_of(&err).stage, error_of(&err).errno),
        (Stage::Exec, libc::ENOENT),
        "{err}"
    );
    assert_eq!(birth.wait().unwrap().code(), Some(127));

    assert!(!mark.exists(), "a target ran after a failed setup");
    assert!(fx.cleanup().complete());
}

#[test]
fn the_child_closes_the_parent_only_endpoints_before_it_is_ready() {
    let mut fx = Fixture::new("child_closes_parent_only_endpoints");
    for closes in [true, false] {
        let (reader, writer) = std::io::pipe().unwrap();
        let mut launch = sh("exec sleep 600");
        if closes {
            launch.close_in_child(writer.as_raw_fd());
        }
        let (mut birth, _) = gated(
            &mut fx,
            launch,
            if closes {
                "closes the writer"
            } else {
                "keeps the writer"
            },
        );
        birth.ready(BOUND).unwrap();
        // The daemon's own copy is the only writer left, if the child closed its copy.
        drop(writer);
        let eof = pipe_hits_eof(&reader, QUIET);
        assert_eq!(
            eof,
            closes,
            "a gated child that {} the endpoint: the reader's EOF was {eof}",
            if closes { "closed" } else { "kept" }
        );
    }
    assert!(fx.cleanup().complete());
}

/// Whether reading `pipe` reaches EOF within `bound` (nothing is ever written to it here).
fn pipe_hits_eof(pipe: &std::io::PipeReader, bound: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd; a hangup (EOF) is reported as POLLHUP.
    let rc = unsafe { libc::poll(&mut pfd, 1, bound.as_millis() as libc::c_int) };
    rc > 0 && pfd.revents & libc::POLLHUP != 0
}

#[test]
fn session_and_group_placement_is_reported_by_the_child_itself() {
    let mut fx = Fixture::new("session_and_group_placement");
    // SAFETY: plain reads of this process's own ids.
    let (my_pgid, my_sid) = unsafe { (libc::getpgrp(), libc::getsid(0)) };

    let (mut left, _) = gated(&mut fx, sh("exec sleep 600"), "leaves the group");
    let ready = left.ready(BOUND).unwrap();
    assert_eq!(
        (ready.pgid, ready.sid),
        (my_pgid, my_sid),
        "a child that leaves its group stays in its parent's"
    );

    let mut launch = sh("exec sleep 600");
    launch.group(Group::Own);
    let (mut own, _) = gated(&mut fx, launch, "own group");
    let own_ready = own.ready(BOUND).unwrap();
    assert_eq!(
        (own_ready.pgid, own_ready.sid),
        (own_ready.pid, my_sid),
        "its own group, the caller's session"
    );

    let mut launch = sh("exec sleep 600");
    launch.group(Group::Join(own_ready.pgid));
    let (mut joined, _) = gated(&mut fx, launch, "joins");
    let joined_ready = joined.ready(BOUND).unwrap();
    assert_eq!(
        joined_ready.pgid, own_ready.pgid,
        "joined the other child's group"
    );

    let mut launch = sh("exec sleep 600");
    launch.new_session(true);
    let (mut session, _) = gated(&mut fx, launch, "own session");
    let session_ready = session.ready(BOUND).unwrap();
    assert_eq!(
        (session_ready.sid, session_ready.pgid),
        (session_ready.pid, session_ready.pid),
        "a session of its own"
    );
    assert!(fx.cleanup().complete());
}

#[test]
fn the_target_gets_the_standard_streams_the_directory_the_environment_and_clean_signals() {
    let dir = tempfile::tempdir().unwrap();
    let mut fx = Fixture::new("target_surroundings");
    let (reader, writer) = std::io::pipe().unwrap();
    let cwd = OwnedFd::from(std::fs::File::open(dir.path()).unwrap());
    // Stdin must be /dev/null (cat ends at once); stdout is the pipe; the signal lines show a clean slate.
    let mut launch =
        sh("cat; pwd; echo \"var=$LAUNCH_VAR\"; grep -E '^Sig(Ign|Blk):' /proc/self/status");
    launch
        .stdio(1, OwnedFd::from(writer))
        .cwd(cwd)
        .env("LAUNCH_VAR", "kept");
    // A disposition this process ignores must not reach the target.
    // SAFETY: restoring the disposition below; SIGUSR1 is otherwise unused by this test binary.
    let previous = unsafe { libc::signal(libc::SIGUSR1, libc::SIG_IGN) };
    let (mut birth, _) = gated(&mut fx, launch, "surroundings");
    // SAFETY: puts back what was there.
    unsafe { libc::signal(libc::SIGUSR1, previous) };
    birth.ready(BOUND).unwrap();
    birth.release().unwrap();
    birth.exec_result(BOUND).unwrap();
    let mut out = String::new();
    std::io::Read::read_to_string(&mut &reader, &mut out).unwrap();
    assert!(birth.wait().unwrap().success(), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap(),
        "the working directory: {out}"
    );
    assert_eq!(lines[1], "var=kept", "{out}");
    assert!(
        lines.contains(&"SigIgn:\t0000000000000000"),
        "an ignored signal reached the target: {out}"
    );
    assert!(
        lines.contains(&"SigBlk:\t0000000000000000"),
        "a blocked signal reached the target: {out}"
    );
    assert!(fx.cleanup().complete());
}

/// A claim taken for a birth: the fence of a fresh state folder.
fn state_with_fence(root: &Path) -> PathBuf {
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    state
}

fn contended(state: &Path) -> bool {
    BirthClaim::take(state).is_err()
}

#[test]
fn the_claim_stays_contended_through_the_parents_close_and_the_exec_until_the_child_ends() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_fence(dir.path());
    let mut fx = Fixture::new("claim_lifetime");

    let claim = BirthClaim::take(&state).expect("the fence is free");
    assert!(
        contended(&state),
        "a held claim is contended from a second descriptor"
    );
    let mut launch = sh("exec sleep 600");
    launch.inherit_across_exec(claim.as_raw_fd());
    let (mut birth, index) = gated(&mut fx, launch, "claim holder");
    birth.ready(BOUND).unwrap();

    drop(claim); // the parent's close: no unlock runs, the child's copy keeps the description locked
    assert!(
        contended(&state),
        "the parent's close released the claim while the child held a copy"
    );
    birth.release().unwrap();
    birth.exec_result(BOUND).unwrap();
    assert!(contended(&state), "the exec released the claim");

    birth.signal(libc::SIGKILL).unwrap();
    assert!(fx.identity(index).exited(BOUND));
    birth.wait().unwrap();
    wait_for(BOUND, "the fence to be free after the holder's end", || {
        BirthClaim::take(&state).ok()
    });
    assert!(fx.cleanup().complete());
}

#[test]
fn a_claim_the_child_was_not_left_does_not_survive_the_exec() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_fence(dir.path());
    let mut fx = Fixture::new("claim_without_inherit");
    let claim = BirthClaim::take(&state).unwrap();
    let (mut birth, _) = gated(&mut fx, sh("exec sleep 600"), "no claim copy");
    birth.ready(BOUND).unwrap();
    birth.release().unwrap();
    birth.exec_result(BOUND).unwrap();
    drop(claim);
    // The descriptor was close-on-exec: nothing of the child holds the description, so the fence is free.
    wait_for(BOUND, "the fence to be free", || {
        BirthClaim::take(&state).ok()
    });
    assert!(fx.cleanup().complete());
}

/// The role a re-run of this test binary plays for [`the_claim_outlives_its_parents_death`]: take the claim, hand it
/// to a child that execs, say who the child is, and die by SIGKILL. A no-op in an ordinary run.
#[test]
fn claim_parent_role() {
    let Some(dir) = std::env::var_os("SOT_L2_ROLE_DIR").map(PathBuf::from) else {
        return;
    };
    sot_log::test_isolated::enter("native_premises::claim_parent_role");
    let claim = BirthClaim::take(&dir.join("state")).expect("the role takes the claim");
    // The child ends when the case writes `stop` or its folder is gone (the case never signals a pid it did not spawn); it
    // ends by itself after ten minutes otherwise.
    let mut launch = sh(&format!(
        "n=0; while [ -d '{0}' ] && [ ! -e '{0}/stop' ] && [ $n -lt 6000 ]; do sleep 0.1; n=$((n+1)); done",
        dir.display()
    ));
    launch.inherit_across_exec(claim.as_raw_fd());
    let mut birth = launch.begin().expect("begin");
    birth.ready(BOUND).expect("ready");
    birth.release().expect("release");
    birth.exec_result(BOUND).expect("exec");
    let created = crate::native::start_ticks(birth.pid()).expect("start time");
    std::fs::write(dir.join("child.txt"), format!("{} {created}", birth.pid())).unwrap();
    // The parent dies without a drop: no close, no unlock, no kill of the child.
    // SAFETY: ends this role process by a signal it sends itself.
    unsafe { libc::raise(libc::SIGKILL) };
}

#[test]
fn the_claim_outlives_its_parents_death() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_fence(dir.path());
    let mut fx = Fixture::new("claim_outlives_parent");
    let (mut command, entry) =
        sot_log::test_isolated::test_command("native_premises::claim_parent_role");
    let mut parent = command
        .env("SOT_L2_ROLE_DIR", dir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the role");
    let parent_status = parent.wait().expect("the role ended");
    assert_eq!(
        parent_status.signal(),
        Some(libc::SIGKILL),
        "the role was meant to die by SIGKILL: {parent_status}"
    );
    entry.assert_once(parent.id());

    let text =
        std::fs::read_to_string(dir.path().join("child.txt")).expect("the role named its child");
    let mut parts = text.split_whitespace().map(|n| n.parse::<u64>().unwrap());
    let (pid, created) = (parts.next().unwrap() as i32, parts.next().unwrap());
    let index = fx
        .watch(pid, Some(created), "the dead parent's child")
        .expect("authority over the orphan");
    assert!(
        !fx.identity(index).exited(Duration::from_millis(100)),
        "the child died with its parent"
    );
    assert!(
        contended(&state),
        "the fence was released by the parent's death while its child lived"
    );

    std::fs::write(dir.path().join("stop"), b"stop").unwrap();
    assert!(fx.identity(index).exited(BOUND));
    wait_for(BOUND, "the fence to be free after the child's end", || {
        BirthClaim::take(&state).ok()
    });
    assert!(fx.cleanup().complete());
}

#[test]
fn adopting_a_claim_checks_the_file_and_that_the_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_fence(dir.path());
    let claim = BirthClaim::take(&state).unwrap();

    // The real thing: a descriptor of the held description, inherited as a duplicate.
    // SAFETY: a plain duplicate of a live descriptor.
    let dup = unsafe { libc::dup(claim.as_raw_fd()) };
    assert!(dup >= 0);
    let adopted = BirthClaim::adopt(dup, &state).expect("a duplicate of the held claim is adopted");
    // SAFETY: reads the flags of the adopted descriptor.
    let flags = unsafe { libc::fcntl(adopted.as_raw_fd(), libc::F_GETFD) };
    assert!(
        flags & libc::FD_CLOEXEC != 0,
        "an adopted claim must not cross another exec"
    );
    drop(claim);
    assert!(contended(&state), "the adopted copy alone holds the fence");
    drop(adopted);

    // Another file: refused.
    let other = std::fs::File::create(dir.path().join("other")).unwrap();
    // SAFETY: a duplicate the adopt call takes ownership of.
    let other_fd = unsafe { libc::dup(other.as_raw_fd()) };
    let err = BirthClaim::adopt(other_fd, &state)
        .err()
        .expect("a descriptor of another file is refused");
    assert!(format!("{err}").contains("not the fence"), "{err}");

    // The fence file, but no lock held by anything: refused.
    let unlocked = std::fs::File::open(state.join("supervisor.lock")).unwrap();
    // SAFETY: a duplicate the adopt call takes ownership of.
    let unlocked_fd = unsafe { libc::dup(unlocked.as_raw_fd()) };
    let err = BirthClaim::adopt(unlocked_fd, &state)
        .err()
        .expect("an unlocked descriptor is refused");
    assert!(format!("{err}").contains("does not hold"), "{err}");
}

/// The role a re-run plays for [`a_child_forks_into_its_parents_group_unless_the_parent_has_its_own_session`]: fork one
/// released child with the launcher and leave it running, report its group, and wait to be ended. A no-op otherwise.
#[test]
fn source_group_role() {
    let Some(dir) = std::env::var_os("SOT_L2_ROLE_DIR").map(PathBuf::from) else {
        return;
    };
    sot_log::test_isolated::enter("native_premises::source_group_role");
    // The child ends when the case writes `stop` in the role's folder or the folder is gone, and by itself after ten minutes.
    let mut birth = sh(&format!(
        "n=0; while [ -d '{0}' ] && [ ! -e '{0}/stop' ] && [ $n -lt 6000 ]; do sleep 0.1; n=$((n+1)); done",
        dir.display()
    ))
    .begin()
    .expect("begin");
    let ready = birth.ready(BOUND).expect("ready");
    birth.release().expect("release");
    birth.exec_result(BOUND).expect("exec");
    let created = crate::native::start_ticks(birth.pid()).expect("start time");
    std::fs::write(
        dir.join("child.txt"),
        format!("{} {created} {}", birth.pid(), ready.pgid),
    )
    .unwrap();
    std::mem::forget(birth); // the child outlives this role; the case ends it by the stop file
    std::thread::sleep(Duration::from_secs(600));
}

#[test]
fn a_child_forks_into_its_parents_group_unless_the_parent_has_its_own_session() {
    let mut fx = Fixture::new("source_group");
    let mut swept_group = 0;
    let mut children = Vec::new();
    let mut folders = Vec::new();
    for (label, own_session) in [
        ("parent in the swept group", false),
        ("parent with its own session", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (command, entry) =
            sot_log::test_isolated::test_command("native_premises::source_group_role");
        let mut launch = Launch::new(command.get_program());
        launch.args(command.get_args());
        for (k, v) in command.get_envs() {
            if let Some(v) = v {
                launch.env(k, v);
            }
        }
        launch.env("SOT_L2_ROLE_DIR", dir.path());
        if own_session {
            launch.new_session(true);
        } else {
            launch.group(Group::Own);
        }
        let mut parent = launch.begin().expect("begin the parent role");
        let ready = parent.ready(BOUND).unwrap();
        let parent_index = fx.own(parent.pid(), label).unwrap();
        parent.release().unwrap();
        parent.exec_result(BOUND).unwrap();
        let text = wait_for(BOUND, "the role to report its child", || {
            std::fs::read_to_string(dir.path().join("child.txt"))
                .ok()
                .filter(|t| t.split_whitespace().count() == 3)
        });
        let nums: Vec<u64> = text
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        let child = fx
            .watch(nums[0] as i32, Some(nums[1]), &format!("child of: {label}"))
            .unwrap();
        // The child is in the group of the parent that forked it: the parent's own, its pid.
        assert_eq!(
            nums[2] as i32, ready.pgid,
            "{label}: the child's group is not its parent's"
        );
        if !own_session {
            swept_group = ready.pgid;
        }
        children.push((label, child, parent_index));
        std::mem::forget(entry); // the role's entry record is not this test's to check
        std::mem::forget(parent);
        folders.push(dir);
    }
    // The sweep of the first parent's group: the child that forked inside it dies, the one that did not, lives.
    // SAFETY: a signal to a group the fixture created and still has a member of (its identity is unreaped).
    unsafe { libc::killpg(swept_group, libc::SIGKILL) };
    assert!(
        fx.identity(children[0].1).exited(BOUND),
        "a child forked inside the swept group survived the sweep"
    );
    assert!(
        !fx.identity(children[1].1)
            .exited(Duration::from_millis(300)),
        "a child forked from a parent with its own session died in the sweep"
    );
    for folder in &folders {
        std::fs::write(folder.path().join("stop"), b"stop").unwrap();
    }
    assert!(fx.cleanup().complete());
}

/// The three places the native branch can be held, each observed from outside while the child waits there. Needs the
/// barrier build of the launcher (`daemon-lifetime-faults`); an installed build compiles the pauses out.
#[cfg(feature = "daemon-lifetime-faults")]
#[test]
fn a_pause_holds_the_child_at_its_stage_until_the_harness_opens_it() {
    use sot_log::host::process_tree::{
        Pause, PAUSE_BEFORE_CLOSE, PAUSE_BEFORE_READY, PAUSE_BEFORE_SESSION,
    };
    let mut fx = Fixture::new("native_pause_stages");
    // SAFETY: plain read of this process's own session id.
    let my_sid = unsafe { libc::getsid(0) };
    for (stage, name) in [
        (PAUSE_BEFORE_CLOSE, "before the close"),
        (PAUSE_BEFORE_SESSION, "before setsid"),
        (PAUSE_BEFORE_READY, "before ready"),
    ] {
        let (reached_r, reached_w) = std::io::pipe().unwrap();
        let (go_r, go_w) = std::io::pipe().unwrap();
        let mut launch = sh("exec sleep 600");
        launch.new_session(true).pause(Pause {
            stage,
            out: OwnedFd::from(reached_w),
            go: OwnedFd::from(go_r),
        });
        let (mut birth, index) = gated(&mut fx, launch, name);
        // The child writes its stage byte on arrival: the byte is what proves it is held there.
        let mut byte = [0u8; 1];
        wait_for(
            BOUND,
            &format!("the child to reach the pause {name}"),
            || {
                let mut pfd = libc::pollfd {
                    fd: reached_r.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd, a zero-length wait.
                (unsafe { libc::poll(&mut pfd, 1, 0) } > 0).then_some(())
            },
        );
        // SAFETY: a one-byte read into a local buffer from a descriptor this test owns.
        assert_eq!(
            unsafe { libc::read(reached_r.as_raw_fd(), byte.as_mut_ptr().cast(), 1) },
            1
        );
        assert_eq!(byte[0] as u32, stage, "the stage byte");
        assert!(
            birth.ready(QUIET).is_err(),
            "the child was ready while held {name}"
        );
        if stage <= PAUSE_BEFORE_SESSION {
            // Held before its setsid: still in the caller's session, the state a daemon that dies now leaves it in.
            // SAFETY: a plain read of another process's session id; the child is unreaped, so the pid is its own.
            assert_eq!(
                unsafe { libc::getsid(birth.pid()) },
                my_sid,
                "the child had its own session {name}"
            );
        }
        (&go_w).write_all(b"g").unwrap();
        let ready = birth.ready(BOUND).expect("ready once the pause is opened");
        assert_eq!(ready.sid, ready.pid, "the session after the setsid {name}");
        assert!(!fx.identity(index).exited(Duration::ZERO));
        drop(birth);
        assert!(fx.identity(index).exited(BOUND));
    }
    assert!(fx.cleanup().complete());
}

/// A barrier nobody opens ends its child: the harness closing its go end is `_exit` before the target.
#[cfg(feature = "daemon-lifetime-faults")]
#[test]
fn a_pause_whose_go_end_closes_ends_the_child_before_its_target() {
    use sot_log::host::process_tree::{Pause, PAUSE_BEFORE_SESSION};
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("mark");
    let mut fx = Fixture::new("native_pause_closed");
    let (_reached_r, reached_w) = std::io::pipe().unwrap();
    let (go_r, go_w) = std::io::pipe().unwrap();
    let mut launch = sh("echo entered > \"$MARK\"");
    launch.env("MARK", &mark).pause(Pause {
        stage: PAUSE_BEFORE_SESSION,
        out: OwnedFd::from(reached_w),
        go: OwnedFd::from(go_r),
    });
    // The go end is the parent's: the child closes its copy, or it would hold its own go pipe open for ever.
    launch.close_in_child(go_w.as_raw_fd());
    let (mut birth, index) = gated(&mut fx, launch, "closed pause");
    std::thread::sleep(QUIET);
    drop(go_w);
    assert!(
        fx.identity(index).exited(BOUND),
        "the child did not end when its go end closed: it holds a copy of that end"
    );
    let status = birth.wait().unwrap();
    assert_eq!(status.code(), Some(125), "{status}");
    assert!(fx.identity(index).exited(BOUND));
    assert!(!mark.exists());
    assert!(fx.cleanup().complete());
}

/// A process the case learned of (a pid a product printed, a fixture wrote, a peer credential reported) is watched, never
/// signalled: its identity refuses a signal and cleanup leaves it alone. The case ends the process it spawned itself.
#[test]
fn a_watched_identity_is_never_signalled() {
    let mut fx = Fixture::new("watched_never_signalled");
    let mut sleeper = std::process::Command::new("sleep")
        .arg("120")
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("start the sleeper");
    let watched = fx
        .watch(sleeper.id() as i32, None, "a watched sleeper")
        .expect("an identity to watch");
    let refused = fx
        .identity(watched)
        .kill()
        .expect_err("a watched identity was signalled");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(cleanup.watched_alive.len(), 1, "{cleanup:?}");
    assert!(
        sleeper.try_wait().unwrap().is_none(),
        "cleanup signalled a watched process"
    );
    // The case ends the process it spawned itself.
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
}
