// topology/store.rs — the daemon's live view of the declared topology file
// (`sot_topology`, grammar v2). ON-DEMAND re-read (mtime+size),
// never a file watcher: plan §B "Editing the master list" is explicit that
// `notify` misses writes on a network filesystem, exactly the reason
// `topology/mod.rs`'s own BOM-incident note already learned the hard way once
// for `[monitor]`.
//
// One `TopologyStore` per daemon (constructed once at startup, pointed at
// `topology::locate()`'s path). `refresh()` is the ONE re-read path, called
// by every `topology.*` op and `version.query`: this is what makes a hand
// edit on the hub behave exactly like a `topology.set` (plan §B), and it's
// also how `topology.set` itself reads the current file before editing and
// confirms the write after — both routes are the same code path.
//
// A currently-malformed file on disk is refused (`error` is `Some`)
// WITHOUT discarding the last good parse: `refresh()` keeps answering the
// stale-but-valid topology rather than losing all state to one bad hand
// edit.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use sot_topology::{self as topology, Topology};

/// `topology.changed` broadcast payload (mirrors `WorkspaceChanged`): one
/// per successful topology write, whether from `topology.set` or a
/// noticed hand edit.
#[derive(Clone, Debug)]
pub struct TopologyChanged {
    pub hash: String,
}

struct Good {
    stat: (Option<SystemTime>, u64),
    topo: Topology,
    hash: String,
}

/// An edit's exclusion over hosts.toml, in this process and across processes; dropping it releases both.
pub struct EditGuard<'a> {
    _thread: std::sync::MutexGuard<'a, ()>,
    _file: sot_platform::host::WriterLock,
}

/// The cross-process lock every writer of the file at `dest` takes: a file beside it, held until the guard drops (the
/// kernel releases it with its holder). Fails closed after the platform's bounded retry.
pub(crate) fn lock_file(dest: &Path) -> Result<sot_platform::host::WriterLock, String> {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".lock");
    let lock = PathBuf::from(name);
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // The lock is taken on a file that exists; it stays, empty, beside hosts.toml.
    std::fs::OpenOptions::new().create(true).append(true).open(&lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    sot_platform::host::lock_writer(&lock).map_err(|e| format!("{}: {e}", lock.display()))
}

pub struct TopologyStore {
    path: PathBuf,
    inner: Mutex<Option<Good>>,
    /// Held by one edit from its read of the file to its write, so two edits never start from the same text.
    edit: Mutex<()>,
}

/// One `refresh()` call's outcome.
pub struct Refreshed {
    /// The topology to use right now — the freshly parsed file, or (on a
    /// currently-malformed file, or one that's since vanished) the last
    /// good parse, if any. `None` only when nothing has ever parsed.
    pub topo: Option<Topology>,
    pub hash: Option<String>,
    /// True only when this call's active hash differs from the PREVIOUS
    /// call's active hash — the one condition that should fire
    /// `topology.changed`. Never true on the very first successful parse
    /// (there is no previous hash for a client to have diverged from; the
    /// initial value rides `version.query`'s `hosts_toml_hash` instead).
    pub changed: bool,
    /// Set when the file currently on disk fails to parse (or can't be
    /// read/stat'd for a reason other than "absent"). `topo`/`hash` still
    /// carry the retained fallback when one exists.
    pub error: Option<String>,
}

impl TopologyStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path, inner: Mutex::new(None), edit: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serializes edits: an edit holds this from its first `refresh` to its write and its confirming `refresh`. The
    /// in-process mutex orders this daemon's edits; the file lock beside hosts.toml orders them against every other
    /// writer of the file (`sotd topology sync` on a shared home, another daemon on it).
    pub fn edit_lock(&self) -> Result<EditGuard<'_>, String> {
        let thread = self.edit.lock().unwrap_or_else(|e| e.into_inner());
        let file = lock_file(&self.path)?;
        Ok(EditGuard { _thread: thread, _file: file })
    }

    pub fn refresh(&self) -> Refreshed {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let fallback = |g: &Option<Good>| match g {
            Some(good) => (Some(good.topo.clone()), Some(good.hash.clone())),
            None => (None, None),
        };

        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => {
                // Absent is not an error here: a fresh box with no
                // hosts.toml yet, or a file that vanished underfoot.
                // Neither replaces a good cache with nothing.
                let (topo, hash) = fallback(&guard);
                return Refreshed { topo, hash, changed: false, error: None };
            }
        };
        let stat_now = (meta.modified().ok(), meta.len());
        if let Some(good) = guard.as_ref() {
            if good.stat == stat_now {
                return Refreshed {
                    topo: Some(good.topo.clone()),
                    hash: Some(good.hash.clone()),
                    changed: false,
                    error: None,
                };
            }
        }

        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) => {
                let (topo, hash) = fallback(&guard);
                return Refreshed { topo, hash, changed: false, error: Some(format!("{}: {e}", self.path.display())) };
            }
        };
        match topology::parse(&text) {
            Err(e) => {
                let (topo, hash) = fallback(&guard);
                Refreshed { topo, hash, changed: false, error: Some(format!("{}: {e}", self.path.display())) }
            }
            Ok(topo) => {
                let hash = topology::hash_text(&text);
                let prev_hash = guard.as_ref().map(|g| g.hash.clone());
                let changed = prev_hash.as_deref().is_some_and(|p| p != hash);
                *guard = Some(Good { stat: stat_now, topo: topo.clone(), hash: hash.clone() });
                Refreshed { topo: Some(topo), hash: Some(hash), changed, error: None }
            }
        }
    }
}

/// tmp + rename (the installer's own pattern — mirrors `topology::cli::
/// write_atomic`, factored here so `topology.set` and `sotd topology sync`
/// share the one implementation).
pub(crate) fn write_atomic(dest: &Path, text: &str) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    // A name of its own, so two writers never share a temporary file.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dest.with_extension(format!("toml.tmp.{}.{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
