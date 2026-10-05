//! A test's write of a comm registry, under the registry lock the daemon and the comm scripts take (the protocol's own
//! link take), so a daemon that stamps the registry on its own cannot race it or lose its update.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Numbers the lock record of each call, so two threads of one process never share a record or its temp.
static CALLS: AtomicU64 = AtomicU64::new(0);

/// Removes `.registry.lock` when dropped, so a panic in `update` does not leave it behind.
struct Held(PathBuf);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Takes the registry lock (waiting up to 10 s), reads `registry.json` (or `{"agents":{}}`), calls `update`, writes
/// the result through a temp file and renames it, then releases the lock. The lock record has no proof fields, the
/// form a box that cannot prove a death writes, so no waiter ever forces it.
pub fn write_registry(comm_root: &Path, update: impl FnOnce(&mut serde_json::Value)) {
    std::fs::create_dir_all(comm_root).expect("mkdir comm root");
    let pid = std::process::id();
    let lock = comm_root.join(".registry.lock");
    let name = format!("test-{}", CALLS.fetch_add(1, Ordering::Relaxed));
    let temp = comm_root.join(format!(".registry.lock.tmp.{name}.-.-.-.{pid}.-"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        std::fs::write(&temp, format!("{name}:-:-:-:{pid}:-\n")).expect("write the lock record");
        let linked = std::fs::hard_link(&temp, &lock);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => break,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&lock).unwrap_or_default();
                assert!(Instant::now() < deadline, "the registry lock stayed held by {:?}", holder.trim());
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("take the registry lock: {e}"),
        }
    }
    let _held = Held(lock);
    let path = comm_root.join("registry.json");
    let mut doc: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({ "agents": {} }));
    update(&mut doc);
    let tmp = comm_root.join("registry.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&doc).expect("encode")).expect("write registry tmp");
    std::fs::rename(&tmp, &path).expect("rename registry");
}
