//! Cross-process staging lock.
//!
//! The backend daemon and the frontend can both stage into one updates root
//! on an all-in-one install — a process-local mutex (the old `STAGE_LOCK`)
//! cannot serialize that. This is the classic portable mkdir lock: `mkdir`
//! is atomic on every filesystem we run on (including NFS, where flock is
//! unreliable), an owner file records who holds it (pid@host plus a random
//! nonce), and stale locks (dead pid on this host, or older than the
//! takeover age) are broken loudly.
//!
//! Race hardening (second Codex review): a stale break RENAMES the observed
//! lock away only after re-verifying that the dir at the path still carries
//! the SAME owner nonce that was observed stale — a delayed breaker can
//! never rename a fresh lock someone re-acquired in between. Release is
//! owner-verified the same way: a lock whose owner file no longer matches
//! our nonce is NOT ours to remove.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};

const LOCK_DIR: &str = ".lock";
/// A lock older than this is presumed abandoned. Generous on purpose: a
/// prepare (git fetch + several Julia instantiates + load-tests + npm) can
/// legitimately hold the lock for hours, and the lock dir's mtime is its
/// creation time — a takeover mid-prepare would corrupt a live stage. Set
/// ABOVE the aggregate of the prepare pipeline's own per-step timeouts, so
/// a live-but-slow holder can never be broken; only a truly wedged or dead
/// one is (self-heals within half a day on a daily-cadence updater).
const TAKEOVER_AGE: Duration = Duration::from_secs(12 * 3600);

/// Held staging lock; released on drop (best-effort) or via `release()`.
#[derive(Debug)]
pub struct StageLock {
    dir: PathBuf,
    /// The exact owner line we wrote; release only removes a lock that still
    /// carries it.
    owner: String,
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
                    let owner = format!(
                        "{}@{}#{}\n",
                        std::process::id(),
                        this_host(),
                        nonce()
                    );
                    let _ = std::fs::write(dir.join("owner"), &owner);
                    return Ok(Self {
                        dir,
                        owner,
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
        // Capture the owner we observed as stale — the break below refuses
        // to touch a lock whose owner has changed since this observation.
        let observed_owner = std::fs::read_to_string(dir.join("owner")).unwrap_or_default();
        let mut stale = age > TAKEOVER_AGE;
        #[cfg(unix)]
        if !stale {
            // Owner format "<pid>@<host>#<nonce>": if it's this host and the
            // pid is gone, the holder died without releasing.
            if let Some((pid_s, host)) = owner_pid_host(&observed_owner) {
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
        if !stale {
            return Ok(false);
        }
        // Re-verify at the last moment: the dir at this path must still carry
        // the exact owner we judged stale. A fresh lock (re-acquired since
        // our observation) has a different nonce and is left alone.
        let current_owner = std::fs::read_to_string(dir.join("owner")).unwrap_or_default();
        if current_owner != observed_owner {
            return Ok(false);
        }
        // Break by RENAME, not remove: rename is atomic, so exactly ONE of
        // several concurrent breakers wins; the loser's rename fails NotFound.
        let graveyard = dir.with_file_name(format!(
            ".lock-stale-{}-{}",
            std::process::id(),
            nonce()
        ));
        match std::fs::rename(dir, &graveyard) {
            Ok(()) => {
                // Post-rename sanity: if what we renamed is somehow NOT the
                // owner we observed (the narrow re-verify → rename window),
                // put it back untouched.
                let grabbed = std::fs::read_to_string(graveyard.join("owner")).unwrap_or_default();
                if grabbed != observed_owner {
                    let _ = std::fs::rename(&graveyard, dir);
                    return Ok(false);
                }
                tracing::warn!(lock = %dir.display(), age_s = age.as_secs(), owner = %observed_owner.trim(), "broke stale staging lock");
                let _ = std::fs::remove_dir_all(&graveyard);
                Ok(true)
            }
            // Lost the race (already broken/re-acquired) — not ours to free.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Release explicitly (preferred over relying on Drop).
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if !self.released {
            // Owner-verified: never remove a lock that is no longer ours
            // (e.g. broken as stale during an extreme stall and re-acquired
            // by someone else).
            let current = std::fs::read_to_string(self.dir.join("owner")).unwrap_or_default();
            if current == self.owner {
                let _ = std::fs::remove_file(self.dir.join("owner"));
                let _ = std::fs::remove_dir(&self.dir);
            } else {
                tracing::warn!(lock = %self.dir.display(), "lock owner changed under us — not releasing someone else's lock");
            }
            self.released = true;
        }
    }
}

impl Drop for StageLock {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Parse "<pid>@<host>" out of an owner line "<pid>@<host>#<nonce>".
#[cfg(unix)]
fn owner_pid_host(owner: &str) -> Option<(&str, &str)> {
    let owner = owner.trim();
    let (pid_host, _nonce) = owner.split_once('#').unwrap_or((owner, ""));
    pid_host.split_once('@')
}

/// This machine's hostname (real, not the `HOSTNAME` shell variable — which
/// bash does not export to non-interactive processes).
fn this_host() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// A cheap uniqueness nonce (monotonic-ish clock nanos + pid mix); good
/// enough to distinguish two lock acquisitions, no crypto needed.
fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (std::process::id() as u128) << 64
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
            format!("4194304@{}#123\n", this_host()),
        )
        .unwrap();
        let l = StageLock::acquire(&root, Duration::from_secs(2)).await.unwrap();
        drop(l);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn release_refuses_foreign_lock() {
        let root = test_root("foreign");
        let _ = std::fs::remove_dir_all(&root);
        let l1 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
        // Simulate a takeover: overwrite the owner file with someone else's.
        std::fs::write(root.join(LOCK_DIR).join("owner"), "999@other#42\n").unwrap();
        l1.release(); // must NOT remove the (now foreign) lock
        assert!(root.join(LOCK_DIR).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
