//! The Linux lifetime guard on real daemons (`lifecycle::daemon_children::guard`): the launched process is the guard and the
//! daemon is its child, the guard ends as the daemon did and forwards what it is sent, a lost guard ends the daemon at once,
//! the boot refuses a second thread, and the relay refresh follows the guard's pid. Every process these cases end is one
//! this test spawned (the launched guard), or the daemon the case's own control connection reports through `SO_PEERCRED`
//! and holds as a pidfd (`native::Identity`); nothing found by walking parent links or reading `ps` is ever signalled.

use crate::fixture_owner::Fixture;
use crate::support::{handoff, poll_until, sotd_command, Env, BOUND, TEST_STATE_HOST};
use crate::SERIAL;
use sot_protocol::ops::{op, FeLeaseReq};
use sot_protocol::{codec, Frame};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A real guarded daemon: its launched process (the guard), the daemon the control connection reported, and its log.
pub struct Run {
    pub env: Env,
    pub log: PathBuf,
    launched: Option<Child>,
    pub daemon: i32,
}

/// The pid `SO_PEERCRED` reports for the process that listens on `socket`: the daemon.
fn peer_pid(socket: &Path) -> Option<i32> {
    let stream = UnixStream::connect(socket).ok()?;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills one ucred of the stated length for the connected socket.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid)
}

/// A process's parent, from `/proc/<pid>/stat`.
pub fn parent_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

impl Run {
    /// The command for a daemon on `env`, its output in `log`. `extra` is set last.
    fn command(env: &Env, log: &Path, extra: &[(&str, &str)]) -> std::process::Command {
        let file = std::fs::File::create(log).expect("create the daemon log");
        let mut cmd = sotd_command();
        cmd.arg("--socket")
            .arg(&env.socket_path)
            .arg("--project-root")
            .arg(&env.daemon_project_root)
            .env("LOCALAPPDATA", &env.state_root)
            .env("XDG_STATE_HOME", &env.state_root)
            .env("XDG_CONFIG_HOME", &env.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
            .env("HOME", &env.home_root)
            .env("USERPROFILE", &env.home_root)
            .env("SOT_COMM_HOME", &env.comm_root)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone().expect("clone the log")))
            .stderr(Stdio::from(file));
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd
    }

    /// Start a daemon and wait until it answers; `own_group` puts the launched process in a process group of its own.
    pub async fn start(tag: &str, extra: &[(&str, &str)], own_group: bool) -> Run {
        Self::boot(Env::new(tag), extra, own_group, false).await
    }

    /// Start a daemon on `env` and wait until it answers: its launched process, the daemon it reported and its log.
    async fn spawn_daemon(
        env: &Env,
        extra: &[(&str, &str)],
        own_group: bool,
        masked: bool,
    ) -> (Child, i32, PathBuf) {
        let log = env._tmp.path().join(format!(
            "daemon-{}.log",
            std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos())
        ));
        let mut cmd = Self::command(env, &log, extra);
        if own_group {
            cmd.process_group(0);
        }
        if masked {
            // SAFETY: the closure runs between fork and exec and makes one async-signal-safe mask call over a local set.
            unsafe {
                cmd.pre_exec(|| {
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                        libc::sigaddset(&mut set, signal);
                    }
                    libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
                    Ok(())
                });
            }
        }
        let launched = cmd.spawn().expect("spawn sotd");
        let socket = env.socket_path.clone();
        let daemon = poll_until(
            || {
                let s = socket.clone();
                async move { peer_pid(&s) }
            },
            BOUND,
            "the daemon to answer",
        )
        .await;
        (launched, daemon, log)
    }

    pub async fn boot(env: Env, extra: &[(&str, &str)], own_group: bool, masked: bool) -> Run {
        let (launched, daemon, log) = Self::spawn_daemon(&env, extra, own_group, masked).await;
        let run = Run {
            env,
            log,
            launched: Some(launched),
            daemon,
        };
        run
    }

    /// The daemon this run started ended (its launched process has been seen to end): start another on the same roots, as
    /// a successor does.
    pub async fn successor(&mut self, extra: &[(&str, &str)]) {
        assert!(
            self.status_within(Duration::from_secs(60)).await.is_some(),
            "the predecessor's launched process has not ended"
        );
        let (launched, daemon, log) = Self::spawn_daemon(&self.env, extra, false, false).await;
        self.launched = Some(launched);
        self.daemon = daemon;
        self.log = log;
    }

    /// The daemon is the child of the launched process, which is the guard.
    pub fn assert_guarded(&self) {
        assert_eq!(
            parent_of(self.daemon),
            Some(self.guard_pid()),
            "the daemon is not the child of the launched process: {}",
            self.said()
        );
    }

    pub fn guard_pid(&self) -> i32 {
        self.launched.as_ref().expect("the launched process").id() as i32
    }

    pub fn said(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The launched process's wait status, once it has ended within `bound`.
    pub async fn status_within(&mut self, bound: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(status) = self
                .launched
                .as_mut()?
                .try_wait()
                .expect("try_wait the launched process")
            {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// SIGKILL to the launched guard: a process this test spawned.
    pub fn kill_guard(&mut self) {
        let _ = self.launched.as_mut().expect("the launched process").kill();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(mut child) = self.launched.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A lease from this process, then the window's close: the daemon shuts down and exits 0.
pub async fn close_by_lease(env: &Env) {
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");
    let lease = FeLeaseReq {
        boot: me.boot,
        pid: me.pid,
        created: me.created,
        token: None,
    };
    let mut conn = handoff(
        &env.socket_path,
        &Frame::req(1, op::FE_LEASE, serde_json::to_value(&lease).unwrap()),
    )
    .await;
    let (reply, _) = codec::read_frame(&mut conn).await.expect("the lease reply");
    assert_eq!(reply.payload["outcome"], "granted", "{:?}", reply.payload);
    // The daemon may end before it answers (the backstop case), so the frame is written and no reply is read.
    let _ = codec::write_frame(
        &mut conn,
        &Frame::req(2, op::FE_LEAVING, serde_json::json!({ "intent": "close" })),
        None,
    )
    .await;
}

#[derive(Clone, Copy, Debug)]
enum Stimulus {
    /// The window closes: the daemon exits 0.
    Close,
    /// The window closes with a one-millisecond shutdown bound and a ready row to end: the backstop exits 1.
    Backstop,
    /// A signal to the daemon, through the pidfd the case holds.
    ToDaemon(i32),
    /// A signal to the launched guard alone.
    ToGuard(i32),
    /// A signal to the launched process's group: both receive it.
    ToGroup(i32),
}

#[tokio::test]
async fn the_guard_mirrors_the_daemon_and_forwards_signals() {
    let _serial = SERIAL.lock().await;
    for stimulus in [
        Stimulus::Close,
        Stimulus::Backstop,
        Stimulus::ToDaemon(libc::SIGKILL),
        Stimulus::ToDaemon(libc::SIGABRT),
        Stimulus::ToGuard(libc::SIGTERM),
        Stimulus::ToGuard(libc::SIGINT),
        Stimulus::ToGuard(libc::SIGHUP),
        Stimulus::ToGroup(libc::SIGTERM),
    ] {
        let extra: &[(&str, &str)] = match stimulus {
            Stimulus::Backstop => &[("SOT_TEST_SHUTDOWN_BOUND_MS", "1")],
            _ => &[],
        };
        let mut run = Run::start("gmir", extra, matches!(stimulus, Stimulus::ToGroup(_))).await;
        run.assert_guarded();
        let mut fx = Fixture::new(&format!("guard_mirrors::{stimulus:?}"));
        let daemon = fx
            .adopt(run.daemon, None, "the daemon")
            .expect("authority over the daemon, reported by its control connection");
        match stimulus {
            Stimulus::Close => close_by_lease(&run.env).await,
            Stimulus::Backstop => {
                // A ready row gives the close real work, so the bound passes before it finishes. The capsule is outside the
                // daemon's lifetime and outlives the exit, so the fixture holds the supervisor the product reports.
                let (mut conn, mut next_id) = connect_and_hello(&run.env.socket_path).await;
                let (_, state_dir) = ready_row(&run.env, &mut conn, &mut next_id, "backstop").await;
                drop(conn);
                let (pid, created) = supervisor_in(&run.env, &state_dir)
                    .await
                    .expect("the capsule's supervisor answers");
                fx.adopt(pid, Some(created), "the capsule's supervisor")
                    .expect("authority over the reported supervisor");
                close_by_lease(&run.env).await;
            }
            Stimulus::ToDaemon(sig) => {
                // SAFETY: pidfd_send_signal is the fixture identity's own kill for SIGKILL; for another signal the same call over the held pidfd.
                fx.identity(daemon)
                    .signal(sig)
                    .expect("signal the daemon through its pidfd");
            }
            // SAFETY: a signal to the guard this test spawned.
            Stimulus::ToGuard(sig) => unsafe {
                libc::kill(run.guard_pid(), sig);
            },
            // SAFETY: a signal to the group of the guard this test spawned and put in a group of its own.
            Stimulus::ToGroup(sig) => unsafe {
                libc::killpg(run.guard_pid(), sig);
            },
        }
        let status = run.status_within(Duration::from_secs(60)).await;
        fx.save("status", format!("{status:?}"));
        fx.save(
            "daemon_gone",
            fx.identity(daemon).exited(Duration::from_secs(2)),
        );
        let said = run.said();
        let cleanup = fx.cleanup();
        drop(run);
        assert!(cleanup.complete(), "{cleanup:?}");
        let status = status
            .unwrap_or_else(|| panic!("{stimulus:?}: the launched process did not end:\n{said}"));
        assert_eq!(
            fx.saved("daemon_gone"),
            Some("true"),
            "{stimulus:?}: the daemon outlived its guard's end"
        );
        match stimulus {
            Stimulus::Close => assert_eq!(status.code(), Some(0), "Close: {status:?}\n{said}"),
            Stimulus::Backstop => {
                assert_eq!(status.code(), Some(1), "the backstop: {status:?}\n{said}")
            }
            // INT and TERM are the daemon's own handled signals (130, 143); any other ends it by the signal itself, and the
            // guard ends the same way.
            Stimulus::ToDaemon(sig) | Stimulus::ToGuard(sig) | Stimulus::ToGroup(sig) => match sig {
                libc::SIGTERM => assert_eq!(status.code(), Some(143), "{stimulus:?}: {status:?}\n{said}"),
                libc::SIGINT => assert_eq!(status.code(), Some(130), "{stimulus:?}: {status:?}\n{said}"),
                _ => assert_eq!(status.signal(), Some(sig), "{stimulus:?}: the launched process did not end as the daemon did: {status:?}\n{said}"),
            },
        }
    }
}

#[tokio::test]
async fn losing_the_guard_ends_the_daemon_at_once() {
    let _serial = SERIAL.lock().await;
    let mut run = Run::start("glost", &[], false).await;
    run.assert_guarded();
    let mut fx = Fixture::new("losing_the_guard");
    let daemon = fx
        .adopt(run.daemon, None, "the daemon")
        .expect("authority over the daemon");
    run.kill_guard();
    fx.save(
        "daemon_gone_within_1s",
        fx.identity(daemon).exited(Duration::from_secs(1)),
    );
    let said = run.said();
    let cleanup = fx.cleanup();
    drop(run);
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(
        fx.saved("daemon_gone_within_1s"),
        Some("true"),
        "the daemon kept serving after its guard was killed:\n{said}"
    );
}

#[tokio::test]
async fn the_prologue_refuses_a_second_thread() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("gthr");
    let log = env._tmp.path().join("daemon.log");
    let mut cmd = Run::command(&env, &log, &[("SOT_TEST_PROLOGUE_THREAD", "1")]);
    let mut launched = cmd.spawn().expect("spawn sotd");
    let began = Instant::now();
    let status = loop {
        if let Some(status) = launched.try_wait().expect("try_wait") {
            break status;
        }
        if began.elapsed() > BOUND {
            let _ = launched.kill();
            let _ = launched.wait();
            panic!(
                "the boot with a second thread did not end:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(status.code(), Some(1), "{status:?}\n{said}");
    assert!(
        said.contains("threads"),
        "the refusal does not name the thread count:\n{said}"
    );
    assert!(
        peer_pid(&env.socket_path).is_none(),
        "a daemon answers after a refused boot"
    );
}

/// A systemctl stand-in for the relay refresh: it logs each call, answers `list-unit-files` with one enabled relay socket
/// and answers `show -p MainPID` with the pid in the file `main-pid-of`: `guard` (its grandparent: the daemon's parent) or
/// `daemon` (its parent). The stub runs as a child of the daemon, so `$PPID` is the daemon.
const STUB_SYSTEMCTL: &str = "#!/bin/sh\nd=${0%/*}\necho \"$*\" >> \"$d/calls\"\ncase \"$*\" in\n  *show*MainPID*) if [ \"$(cat \"$d/main-pid-of\")\" = guard ]; then read -r _ _ _ pp _ < \"/proc/$PPID/stat\"; echo \"$pp\"; else echo \"$PPID\"; fi ;;\n  *list-unit-files*) echo 'sot-host-relay-remote-a.socket enabled enabled' ;;\nesac\nexit 0\n";

/// Start a hub-configured daemon whose `systemctl` is the stub and report the stub's calls after the daemon has had time to
/// refresh. `main_pid_of` is what the stub reports as the unit's MainPID.
async fn refresh_calls(main_pid_of: &str) -> String {
    let env = Env::new("grel");
    let bin = env._tmp.path().join("stub-bin");
    std::fs::create_dir_all(&bin).unwrap();
    crate::native_stub_systemctl(&bin, STUB_SYSTEMCTL);
    std::fs::write(bin.join("main-pid-of"), main_pid_of).unwrap();
    let hosts = env._tmp.path().join("hosts.toml");
    std::fs::write(
        &hosts,
        "hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.remote-a]\ndaemon = true\n",
    )
    .unwrap();
    std::fs::create_dir_all(env.config_root.join("systemd/user")).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let log = env._tmp.path().join("daemon.log");
    let extra = [
        ("SOT_HOSTS", hosts.to_str().unwrap()),
        ("SOT_SELF_HOST", "hub-box"),
        ("PATH", path.as_str()),
    ];
    let mut launched = Run::command(&env, &log, &extra)
        .spawn()
        .expect("spawn sotd");
    let socket = env.socket_path.clone();
    poll_until(
        || {
            let s = socket.clone();
            async move { peer_pid(&s) }
        },
        BOUND,
        "the daemon to answer",
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let calls = std::fs::read_to_string(bin.join("calls")).unwrap_or_default();
    let _ = launched.kill();
    let _ = launched.wait();
    calls
}

#[tokio::test]
async fn the_relay_refresh_follows_the_guard() {
    let _serial = SERIAL.lock().await;
    let acted = refresh_calls("guard").await;
    assert!(
        acted.contains("list-unit-files"),
        "the refresh did not run when the unit's MainPID was the guard:\n{acted}"
    );
    let skipped = refresh_calls("daemon").await;
    assert!(
        skipped.contains("MainPID"),
        "the daemon did not ask for the unit's MainPID:\n{skipped}"
    );
    assert!(
        !skipped.contains("list-unit-files"),
        "the refresh ran when the unit's MainPID was the daemon, not the guard:\n{skipped}"
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Ephemerals started through the daemon's real routes, and their end
// ---------------------------------------------------------------------------------------------------------------------

use crate::routes::{adopt_tree, all_ended, julia_bin, ready_row, spin_in_repl, supervisor_in};
use crate::support::connect_and_hello;
use crate::tree::{session_members, Tree};

/// A daemon with a ready row whose REPL has started a tree detached and spins; the tree's identities are held.
pub struct Spinning {
    pub run: Run,
    pub state_dir: PathBuf,
    pub ids: Vec<usize>,
    pub task: tokio::task::JoinHandle<()>,
}

pub async fn start_spinning(tag: &str, fx: &mut Fixture, forking: bool) -> Spinning {
    start_spinning_with(tag, fx, forking, &[]).await
}

/// [`start_spinning`] with more daemon variables.
pub async fn start_spinning_with(
    tag: &str,
    fx: &mut Fixture,
    forking: bool,
    extra: &[(&str, &str)],
) -> Spinning {
    let julia = julia_bin();
    let mut vars = vec![("SOT_JULIA_BIN", julia.as_str())];
    vars.extend_from_slice(extra);
    let run = Run::start(tag, &vars, false).await;
    run.assert_guarded();
    let (mut conn, mut next_id) = connect_and_hello(&run.env.socket_path).await;
    let (workspace_id, state_dir) = ready_row(&run.env, &mut conn, &mut next_id, "repl").await;
    drop(conn);
    let tree = Tree::new(run.env._tmp.path(), "repl-tree");
    let task = spin_in_repl(
        &run.env.socket_path,
        &workspace_id,
        tree.julia_cell(forking),
    )
    .await;
    let ids = adopt_tree(fx, &tree, forking, "the REPL's").await;
    Spinning {
        run,
        state_dir,
        ids,
        task,
    }
}

#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn the_drain_outlasts_a_forking_child() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("drain_outlasts_a_forking_child");
    let spinning = start_spinning("gdrn", &mut fx, true).await;
    let leader = fx.identity(spinning.ids[0]).pid;
    let alive_before = session_members(leader).len();
    let daemon = fx
        .adopt(spinning.run.daemon, None, "the daemon")
        .expect("authority over the daemon");
    spinning.task.abort();
    fx.identity(daemon)
        .kill()
        .expect("SIGKILL the daemon through its pidfd");
    fx.save(
        "leader_gone",
        all_ended(&fx, &spinning.ids, Duration::from_secs(10)),
    );
    // Every process of the leader's session, young children included.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !session_members(leader).is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fx.save("session_left", format!("{:?}", session_members(leader)));
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert!(alive_before >= 1, "the forking tree never ran");
    assert_eq!(
        fx.saved("leader_gone"),
        Some("true"),
        "the forking tree's leader outlived the daemon"
    );
    assert_eq!(
        fx.saved("session_left"),
        Some("[]"),
        "processes of the forking tree's session outlived the daemon"
    );
}

#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn the_guard_ends_only_its_own_subtree() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("guard_ends_only_its_own_subtree");
    let mut a = start_spinning("gsua", &mut fx, false).await;
    let b = start_spinning("gsub", &mut fx, false).await;
    // A process the case starts outside both daemons: a child of this test, ended through its own handle.
    let mut outside = std::process::Command::new("sleep")
        .arg("3160")
        .stdin(Stdio::null())
        .spawn()
        .expect("start the outside process");
    // The capsule's supervisor, as daemon A's own lane reports it (the lane is found through A's runtime folder).
    let supervisor = supervisor_in(&a.run.env, &a.state_dir)
        .await
        .expect("daemon A's supervisor answers");
    let capsule = fx
        .adopt(
            supervisor.0,
            Some(supervisor.1),
            "daemon A's capsule supervisor",
        )
        .expect("authority over the reported supervisor");

    let daemon = fx
        .adopt(a.run.daemon, None, "daemon A")
        .expect("authority over daemon A");
    a.task.abort();
    fx.identity(daemon)
        .kill()
        .expect("SIGKILL daemon A through its pidfd");
    fx.save(
        "a_tree_ended",
        all_ended(&fx, &a.ids, Duration::from_secs(10)),
    );
    // The guard ends after its drain, so what it was going to end is ended by now.
    fx.save(
        "a_guard_ended",
        a.run.status_within(Duration::from_secs(60)).await.is_some(),
    );
    fx.save(
        "b_tree_alive",
        b.ids
            .iter()
            .all(|i| !fx.identity(*i).exited(Duration::ZERO)),
    );
    // `try_wait` reaps a process that has ended, so a zombie does not read as alive.
    fx.save(
        "outside_alive",
        outside
            .try_wait()
            .expect("try_wait the outside process")
            .is_none(),
    );
    fx.save(
        "capsule_alive",
        !fx.identity(capsule).exited(Duration::from_secs(1)),
    );
    let cleanup = fx.cleanup();
    let _ = outside.kill();
    let _ = outside.wait();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(
        fx.saved("a_tree_ended"),
        Some("true"),
        "daemon A's tree outlived it"
    );
    assert_eq!(
        fx.saved("a_guard_ended"),
        Some("true"),
        "daemon A's guard did not end after its drain"
    );
    assert_eq!(
        fx.saved("b_tree_alive"),
        Some("true"),
        "daemon A's guard ended daemon B's tree"
    );
    assert_eq!(
        fx.saved("outside_alive"),
        Some("true"),
        "daemon A's guard ended a process outside its subtree"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "daemon A's guard ended the capsule it started"
    );
}
