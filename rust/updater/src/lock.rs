//! Cross-process staging lock.
//!
//! The backend daemon and the frontend can both stage into one updates root
//! on an all-in-one install — a process-local mutex (the old `STAGE_LOCK`)
//! cannot serialize that (Codex review, MUST-FIX 6). This is the classic
//! portable mkdir lock: `mkdir` is atomic on every filesystem we run on
//! (including NFS, where flock is unreliable), the owner file records who
//! holds it, and stale locks (dead pid on this host, or older than the
//! takeover age) are broken loudly.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};

const LOCK_DIR: &str = ".lock";
/// A lock older than this is presumed abandoned. Generous on purpose: a
/// prepare (git fetch + several Julia instantiates + load-tests) can
/// legitimately hold the lock for a long time, and the lock dir's mtime is
/// its creation time — a takeover mid-prepare would corrupt a live stage.
/// The updater runs on a daily cadence; a wedged holder self-heals within
/// six hours.
const TAKEOVER_AGE: Duration = Duration::from_secs(6 * 3600);

/// Held staging lock; released on drop (best-effort) or via `release()`.
#[derive(Debug)]
pub struct StageLock {
    dir: PathBuf,
    released: bool,
}

impl StageLock {
    /// Acquire the lock under `root`, waiting up to `wait` (polling). Breaks a
    /// stale lock (dead same-host pid, or mtime beyond the takeover age).
    pub async fn acquire(root: &Path, wait: Duration) -> Result<Self> {
        tokio::fs::create_dir_all(root)
            .await
            .with_context(|| format!("creating updates root {}", root.display()))?;
        let dir = root.join(LOCK_DIR);
        let deadline = SystemTime::now() + wait;
        loop {
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    let owner = format!("{}@{}\n", std::process::id(), this_host());
                    let _ = std::fs::write(dir.join("owner"), owner);
                    return Ok(Self {
                        dir,
                        released: false,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::break_if_stale(&dir)? {
                        continue; // freed — retry the create immediately
                    }
                    if SystemTime::now() >= deadline {
                        bail!(
                            "staging lock {} held by another process (owner: {})",
                            dir.display(),
                            std::fs::read_to_string(dir.join("owner"))
                                .unwrap_or_else(|_| "unknown".into())
                                .trim()
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("creating lock dir {}", dir.display()))
                }
            }
        }
    }

    /// Break the lock if its holder is provably gone: same-host pid that no
    /// longer exists, or a lock dir older than the takeover age. Returns true
    /// when it freed the lock.
    fn break_if_stale(dir: &Path) -> Result<bool> {
        let meta = match std::fs::metadata(dir) {
            Ok(m) => m,
            // Vanished between our create attempt and now — treat as freed.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(e).context("statting lock dir"),
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or(Duration::ZERO);
        let mut stale = age > TAKEOVER_AGE;
        #[cfg(unix)]
        if !stale {
            // Owner format "<pid>@<host>": if it's this host and the pid is
            // gone, the holder died without releasing.
            if let Ok(owner) = std::fs::read_to_string(dir.join("owner")) {
                let owner = owner.trim();
                if let Some((pid_s, host)) = owner.split_once('@') {
                    // Only trust a pid probe when both sides know their host —
                    // "unknown" == "unknown" across two NFS-sharing machines
                    // must not read as same-host.
                    if host == this_host() && host != "unknown" {
                        if let Ok(pid) = pid_s.parse::<i32>() {
                            if !pid_alive(pid) {
                                stale = true;
                            }
                        }
                    }
                }
            }
        }
        if stale {
            tracing::warn!(lock = %dir.display(), age_s = age.as_secs(), "breaking stale staging lock");
            let _ = std::fs::remove_file(dir.join("owner"));
            match std::fs::remove_dir(dir) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
                // Someone else may have re-acquired between our checks; not stale anymore.
                Err(_) => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    /// Release explicitly (preferred over relying on Drop).
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if !self.released {
            let _ = std::fs::remove_file(self.dir.join("owner"));
            let _ = std::fs::remove_dir(&self.dir);
            self.released = true;
        }
    }
}

impl Drop for StageLock {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// This machine's hostname (real, not the `HOSTNAME` shell variable — which
/// bash does not export to non-interactive processes).
fn this_host() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// Liveness probe without a libc dependency: /proc on Linux, `kill -0` via sh
/// elsewhere. Only ever called on unix for same-host pids.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::process::Command::new("sh")
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sot-updater-lock-{label}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn lock_excludes_and_releases() {
        let root = test_root("basic");
        let _ = std::fs::remove_dir_all(&root);
        let l1 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
        // Second acquire times out while held.
        assert!(StageLock::acquire(&root, Duration::from_millis(200)).await.is_err());
        l1.release();
        // Freed — acquirable again.
        let l2 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
        drop(l2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dead_pid_lock_is_broken() {
        let root = test_root("stale");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(LOCK_DIR)).unwrap();
        // Pid 4194304 is beyond default pid_max; certainly dead.
        std::fs::write(
            root.join(LOCK_DIR).join("owner"),
            format!("4194304@{}\n", this_host()),
        )
        .unwrap();
        let l = StageLock::acquire(&root, Duration::from_secs(2)).await.unwrap();
        drop(l);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
