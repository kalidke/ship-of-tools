//! child_signal.rs — the one process-wide `fired` flag every child owner
//! selects on, because `kill_on_drop` does not run at `process::exit`.
//! [`Signal::spawn`] and [`Signal::spawn_std`] start a child in its own
//! containment and [`Held`] is the kill: firing the signal, or releasing or
//! dropping the `Held`, ends the child and everything it started, and the
//! child is reaped only after.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio::sync::watch;

/// One shutdown signal, the count of children still alive under it, and the
/// trees it will kill when it fires. The process has one ([`fire`],
/// [`fired`], [`process`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    live: AtomicUsize,
    /// Every contained child's tree by id; `None` once fired, so a spawn
    /// after the fire is killed at once.
    trees: Mutex<Option<HashMap<u64, crate::lifecycle::contain::Tree>>>,
    next: AtomicU64,
}

impl Signal {
    pub(crate) fn new() -> Self {
        Signal {
            fired: watch::channel(false).0,
            live: AtomicUsize::new(0),
            trees: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(0),
        }
    }

    /// The flag goes first, so an owner that sees its child die already
    /// reads [`is_fired`](Self::is_fired); then every tree is killed, by
    /// dropping the registry under its lock, whatever each owner is
    /// awaiting.
    pub(crate) fn fire(&self) {
        self.fired.send_replace(true);
        let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        let taken = trees.take();
        drop(taken);
        drop(trees);
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    /// Children whose [`Held`] is alive.
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// The process-group number of every tree still held.
    #[cfg(all(test, unix))]
    pub(crate) fn held_groups(&self) -> Vec<i32> {
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        trees.as_ref().map(|map| map.values().map(|t| t.pgid()).collect()).unwrap_or_default()
    }

    /// Register `tree`. Once the signal has fired the tree is killed at once
    /// and refused.
    fn hold(&'static self, tree: crate::lifecycle::contain::Tree) -> std::io::Result<Held> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        match trees.as_mut() {
            Some(map) => {
                map.insert(id, tree);
            }
            None => {
                drop(tree);
                return Err(std::io::Error::other("the daemon is shutting down"));
            }
        }
        drop(trees);
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(Held { sig: self, id })
    }

    /// Start `cmd` in its own containment and register the tree. The owner
    /// keeps the returned [`Contained`]: waiting for it, killing it,
    /// dropping it, or [`fire`](Self::fire) ends the child and everything it
    /// started. Once the signal has fired the child is killed at once and
    /// refused.
    pub(crate) fn spawn(&'static self, cmd: &mut tokio::process::Command) -> std::io::Result<Contained> {
        // Made before the spawn, so no exit goes unseen by `Contained::wait`.
        #[cfg(unix)]
        let sigchld = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        crate::lifecycle::contain::prepare(cmd.as_std_mut());
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let tree = match crate::lifecycle::contain::adopt(
            child.id(),
            #[cfg(windows)]
            child.raw_handle(),
        ) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.start_kill();
                return Err(e);
            }
        };
        let held = match self.hold(tree) {
            Ok(held) => held,
            Err(e) => {
                let _ = child.start_kill();
                return Err(e);
            }
        };
        Ok(Contained {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            held,
            child,
            #[cfg(unix)]
            sigchld,
        })
    }

    /// [`spawn`](Self::spawn) for a blocking caller, which keeps the
    /// `Child` itself: the [`Held`] ends the tree when released or dropped,
    /// and the caller reaps the child only after that.
    pub(crate) fn spawn_std(
        &'static self,
        cmd: &mut std::process::Command,
    ) -> std::io::Result<(std::process::Child, Held)> {
        crate::lifecycle::contain::prepare(cmd);
        let mut child = cmd.spawn()?;
        let tree = match crate::lifecycle::contain::adopt(
            Some(child.id()),
            #[cfg(windows)]
            Some(std::os::windows::io::AsRawHandle::as_raw_handle(&child)),
        ) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        match self.hold(tree) {
            Ok(held) => Ok((child, held)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    /// `Command::output` for a contained one-shot: stdin is null, and stdout
    /// and stderr are each read to their end on a thread of their own. The
    /// leader's exit is seen unreaped; then the [`Held`] drops, which kills
    /// the tree and closes any pipe a descendant held; then the child is
    /// reaped, and then both readers are joined. No bound is added, as
    /// `output` has none.
    pub(crate) fn output(&'static self, cmd: &mut std::process::Command) -> std::io::Result<std::process::Output> {
        use std::process::Stdio;
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let (mut child, held) = self.spawn_std(cmd)?;
        let stdout = read_to_end_on_a_thread(child.stdout.take());
        let stderr = read_to_end_on_a_thread(child.stderr.take());
        let seen = crate::lifecycle::contain::exited(&mut child, true);
        drop(held);
        let status = child.wait();
        let (stdout, stderr) = (stdout.join(), stderr.join());
        seen?;
        Ok(std::process::Output { status: status?, stdout: joined(stdout)?, stderr: joined(stderr)? })
    }
}

/// `pipe` read to its end on a thread of its own.
fn read_to_end_on_a_thread<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes)?;
        }
        Ok(bytes)
    })
}

fn joined(read: std::thread::Result<std::io::Result<Vec<u8>>>) -> std::io::Result<Vec<u8>> {
    read.map_err(|_| std::io::Error::other("a pipe reader panicked"))?
}

/// One tree in a [`Signal`]'s registry, and a count of one live child.
/// [`release`](Self::release), or dropping it, kills the tree under the
/// registry lock if the shutdown has not already; the child's leader is
/// reaped only after that, because its pid is the group's number.
pub(crate) struct Held {
    sig: &'static Signal,
    id: u64,
}

impl Held {
    pub(crate) fn release(&self) {
        let mut trees = self.sig.trees.lock().unwrap_or_else(|e| e.into_inner());
        let tree = trees.as_mut().and_then(|map| map.remove(&self.id));
        drop(tree);
        drop(trees);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.release();
        self.sig.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One contained child with its pipes. It owns the `Child`: only
/// [`wait`](Self::wait), [`kill`](Self::kill) and dropping reap it, each
/// after the tree's kill, so a kill never reaches a group number the leader's
/// reap freed. `held` drops before `child`, so a drop kills the tree first.
pub(crate) struct Contained {
    pub(crate) stdin: Option<tokio::process::ChildStdin>,
    pub(crate) stdout: Option<tokio::process::ChildStdout>,
    pub(crate) stderr: Option<tokio::process::ChildStderr>,
    held: Held,
    child: tokio::process::Child,
    #[cfg(unix)]
    sigchld: tokio::signal::unix::Signal,
}

impl Contained {
    /// Wait for the leader to exit, kill what it started, then reap it. A
    /// descendant that holds the child's pipes open is killed too.
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // Seen unreaped, so the group number stays the leader's.
                while !crate::lifecycle::contain::exited_pid(pid, false)? {
                    self.sigchld.recv().await;
                }
            }
            self.held.release();
            self.child.wait().await
        }
        #[cfg(windows)]
        {
            let status = self.child.wait().await?;
            self.held.release();
            Ok(status)
        }
    }

    /// Kill the tree, then the child, and reap it.
    pub(crate) async fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.held.release();
        let _ = self.child.start_kill();
        self.child.wait().await
    }
}

/// Put `SIGCHLD` back to its default disposition. An inherited `SIG_IGN`
/// makes the kernel reap every child at its exit, so a contained leader
/// would not stay a zombie holding its pid, and the pid is its group's
/// number: the invariant is that a contained leader stays a zombie until its
/// tree is killed. Called first thing in the daemon's `main`.
#[cfg(unix)]
pub(crate) fn reset_child_signal() {
    // SAFETY: setting a disposition to SIG_DFL runs no handler code.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// The daemon's one signal; every production owner is given this.
pub(crate) fn process() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(Signal::new)
}

/// Fire the shutdown: every contained child's tree is killed. It stays fired for the life of the process.
pub(crate) fn fire() {
    process().fire()
}

/// Resolves once the shutdown has fired; at once if it already has.
pub(crate) async fn fired() {
    process().fired().await
}

/// Children whose [`Held`] is alive.
pub(crate) fn live_children() -> usize {
    process().live()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A pid is gone once a probe fails.
    #[cfg(unix)]
    fn gone(pid: i32) -> bool {
        (0..150).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes the pid.
            unsafe { libc::kill(pid, 0) != 0 }
        })
    }

    /// An owner stuck writing to a child that never reads cannot poll the
    /// signal; the fire still takes the child and its grandchild.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_kills_a_tree_whose_owner_is_blocked() {
        use tokio::io::AsyncWriteExt;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 3104 & echo $!; exec sleep 3104"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        assert_eq!(signal.live(), 1);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(contained.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let grandchild: i32 = line.trim().parse().expect("grandchild pid");
        let mut stdin = contained.stdin.take().unwrap();
        let owner = tokio::spawn(async move {
            let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
            let _ = contained.wait().await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal.fire();
        tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the blocked owner did not return")
            .expect("owner task");
        assert!(gone(grandchild), "the grandchild survived the shutdown");
        assert_eq!(signal.live(), 0);
    }

    /// A spawn after the fire is killed at once and refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_after_fire_is_refused_and_killed() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        signal.fire();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("f");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!("echo $$ > {}; exec sleep 3105", pid_file.display())]);
        assert!(signal.spawn(&mut cmd).is_err(), "a spawn after the fire was accepted");
        assert_eq!(signal.live(), 0);
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&pid_file).map(|s| s.trim().is_empty()).unwrap_or(true) {
            if began.elapsed() > Duration::from_secs(2) {
                return; // killed before it wrote its pid: nothing left to check
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(gone(pid), "the refused child survived");
    }

    /// The leader's exit takes everything it started with it, before the
    /// leader is reaped, even though nothing fired the signal.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_kills_the_tree_at_the_leaders_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"]).stdout(std::process::Stdio::piped());
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(contained.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let descendant: i32 = line.trim().parse().expect("descendant pid");
        contained.wait().await.expect("wait");
        assert!(gone(descendant), "the leader's descendant survived its exit");
        assert!(signal.held_groups().is_empty(), "a reaped leader's tree is still held");
        drop(contained);
        assert_eq!(signal.live(), 0);
    }

    /// A blocking caller sees the exit unreaped: the leader's number is still
    /// its own and the tree is still held, until the caller lets go of the
    /// `Held`; only then is it reaped.
    #[cfg(unix)]
    fn one_shot_takes_its_tree_after_it_exits() {
        use std::io::BufRead;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"]).stdout(std::process::Stdio::piped());
        let (mut child, held) = signal.spawn_std(&mut cmd).expect("spawn_std");
        let pgid = child.id() as i32;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let descendant: i32 = line.trim().parse().expect("descendant pid");
        assert!(crate::lifecycle::contain::exited(&mut child, true).expect("exited"), "the child had not exited");
        assert!(signal.held_groups().contains(&pgid), "the tree was released before its owner let go");
        // SAFETY: signal 0 only probes the pid.
        assert_eq!(unsafe { libc::kill(pgid, 0) }, 0, "the exit was seen by reaping it");
        drop(held);
        child.wait().expect("wait");
        assert!(gone(descendant), "the one-shot's descendant survived");
    }

    /// A one-shot's output is read to its end, and the tree dies after the
    /// leader's exit: a descendant the exit leaves behind does not survive,
    /// and nothing stays registered or counted.
    #[cfg(unix)]
    #[test]
    fn output_takes_the_tree_after_the_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 3111 >/dev/null 2>&1 & echo $!"]);
        let out = signal.output(&mut cmd).expect("output");
        assert!(out.status.success());
        let descendant: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("descendant pid");
        assert!(gone(descendant), "the one-shot's descendant survived");
        assert!(signal.held_groups().is_empty(), "a reaped leader's tree is still held");
        assert_eq!(signal.live(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_one_shot_takes_its_tree_after_it_exits() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        one_shot_takes_its_tree_after_it_exits();
    }

    /// A parent that started the daemon with `SIGCHLD` ignored must not make
    /// the kernel reap contained leaders; the startup reset undoes it. The
    /// disposition is process-wide, so the scenario runs in a re-executed copy
    /// of this test binary and no test in this process touches `SIGCHLD`.
    #[cfg(unix)]
    #[test]
    fn an_ignored_sigchld_is_reset_at_startup() {
        let name = "lifecycle::child_signal::tests::ignored_sigchld_scenario";
        let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env("SOT_TEST_SIGCHLD_CHILD", "1")
            .output()
            .expect("re-execute the test binary");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child run failed:\n{text}");
        assert!(text.contains(&format!("test {name} ... ok")), "the scenario did not run:\n{text}");
    }

    /// The re-executed half of `an_ignored_sigchld_is_reset_at_startup`; it
    /// does nothing in an ordinary run.
    #[cfg(unix)]
    #[test]
    fn ignored_sigchld_scenario() {
        if std::env::var_os("SOT_TEST_SIGCHLD_CHILD").is_none() {
            return;
        }
        // SAFETY: a plain disposition change in a process of its own.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        reset_child_signal();
        one_shot_takes_its_tree_after_it_exits();
    }
}
