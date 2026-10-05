//! child_signal.rs — the one process-wide `fired` flag every child owner
//! selects on, because `kill_on_drop` does not run at `process::exit`.
//! [`Signal::spawn`] starts a child in its own containment and [`Contained`]
//! is the kill: firing the signal, or dropping the `Contained`, ends the
//! child and everything it started. [`ChildGuard`] only counts the ssh
//! children of the hub link and the topology dial, which are not contained.

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
    /// dropping the registry, whatever each owner is awaiting.
    pub(crate) fn fire(&self) {
        self.fired.send_replace(true);
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(trees);
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    pub(crate) fn guard(&'static self) -> ChildGuard {
        self.live.fetch_add(1, Ordering::SeqCst);
        ChildGuard(&self.live)
    }

    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Start `cmd` in its own containment and register the tree. The owner
    /// keeps the returned [`Contained`] beside the `Child`: dropping it, or
    /// [`fire`](Self::fire), kills the child and everything it started. Once
    /// the signal has fired the child is killed at once and refused.
    pub(crate) fn spawn(
        &'static self,
        cmd: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, Contained)> {
        crate::lifecycle::contain::prepare(cmd);
        let mut child = cmd.spawn()?;
        let tree = match crate::lifecycle::contain::adopt(&child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.start_kill();
                return Err(e);
            }
        };
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let refused = {
            let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
            match trees.as_mut() {
                Some(map) => {
                    map.insert(id, tree);
                    None
                }
                None => Some(tree),
            }
        };
        if let Some(tree) = refused {
            drop(tree);
            return Err(std::io::Error::other("the daemon is shutting down"));
        }
        Ok((child, Contained { _live: self.guard(), id, sig: self }))
    }
}

/// One contained child, alive until dropped. Dropping it kills the child's
/// tree if the shutdown has not already, then stops counting the child.
pub(crate) struct Contained {
    sig: &'static Signal,
    id: u64,
    _live: ChildGuard,
}

impl Drop for Contained {
    fn drop(&mut self) {
        let tree = self
            .sig
            .trees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|map| map.remove(&self.id));
        drop(tree);
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

/// Children whose [`ChildGuard`] is still alive.
pub(crate) fn live_children() -> usize {
    process().live()
}

/// Counts one child alive until dropped. Its owner keeps it beside the
/// `Child`, so it drops once the child is reaped. [`Contained`] holds one;
/// the uncontained ssh owners (the hub link, the topology dial) hold it directly.
pub(crate) struct ChildGuard(&'static AtomicUsize);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn child_guard_killed_on_fire() {
        // A private signal: firing the process's own would kill every other
        // test's children.
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut child = tokio::process::Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let guard = signal.guard();
        assert_eq!(signal.live(), 1);
        let owner = tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {
                _ = child.wait() => {}
                _ = signal.fired() => { let _ = child.kill().await; }
            }
            child.try_wait().ok().flatten()
        });
        signal.fire();
        let status = tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the child was not reaped within 3 s")
            .expect("owner task");
        assert!(status.is_some(), "the child was killed but not reaped");
        assert_eq!(signal.live(), 0);
    }

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
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 3104 & echo $!; exec sleep 3104"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let (mut child, contained) = signal.spawn(&mut cmd).expect("spawn");
        assert_eq!(signal.live(), 1);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(child.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let grandchild: i32 = line.trim().parse().expect("grandchild pid");
        let mut stdin = child.stdin.take().unwrap();
        let owner = tokio::spawn(async move {
            let _contained = contained;
            let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
            let _ = child.wait().await;
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
        let mut cmd = tokio::process::Command::new("sh");
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
}
