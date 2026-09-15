// topology_store.rs — the daemon's live view of the declared topology file
// (`sot_protocol::topology`, grammar v2). ON-DEMAND re-read (mtime+size),
// never a file watcher: plan §B "Editing the master list" is explicit that
// `notify` misses writes on a network filesystem, exactly the reason
// `topology.rs`'s own BOM-incident note already learned the hard way once
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

use sot_protocol::topology::{self, Topology};

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

pub struct TopologyStore {
    path: PathBuf,
    inner: Mutex<Option<Good>>,
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
        Self { path, inner: Mutex::new(None) }
    }

    pub fn path(&self) -> &Path {
        &self.path
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

/// tmp + rename (the installer's own pattern — mirrors `topology_cli::
/// write_atomic`, factored here so `topology.set` and `sotd topology sync`
/// share the one implementation).
pub(crate) fn write_atomic(dest: &Path, text: &str) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = dest.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, dest).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn absent_file_is_not_an_error() {
        let dir = tempdir();
        let store = TopologyStore::new(dir.join("hosts.toml"));
        let r = store.refresh();
        assert!(r.topo.is_none());
        assert!(r.hash.is_none());
        assert!(!r.changed);
        assert!(r.error.is_none());
    }

    #[test]
    fn first_good_parse_is_not_reported_as_changed() {
        let dir = tempdir();
        let path = dir.join("hosts.toml");
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n");
        let store = TopologyStore::new(path);
        let r = store.refresh();
        assert!(r.topo.is_some());
        assert!(!r.changed, "no previous hash to have diverged from");
        assert!(r.error.is_none());
    }

    #[test]
    fn unchanged_stat_short_circuits_and_reports_unchanged() {
        let dir = tempdir();
        let path = dir.join("hosts.toml");
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n");
        let store = TopologyStore::new(path);
        store.refresh();
        let r2 = store.refresh();
        assert!(!r2.changed);
        assert_eq!(r2.hash, store.refresh().hash);
    }

    #[test]
    fn content_change_is_reported_exactly_once() {
        let dir = tempdir();
        let path = dir.join("hosts.toml");
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n");
        let store = TopologyStore::new(path.clone());
        store.refresh();
        // Bump mtime forward so a fast test run's stat comparison sees a
        // real difference even on a coarse filesystem clock.
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n\n[host.b]\ndaemon = true\n");
        bump_mtime(&path);
        let r = store.refresh();
        assert!(r.changed, "content actually changed");
        assert_eq!(r.topo.unwrap().hosts.len(), 2);
        let r2 = store.refresh();
        assert!(!r2.changed, "must not re-fire on the next refresh with no further edit");
    }

    #[test]
    fn malformed_file_is_refused_without_losing_previous_content() {
        let dir = tempdir();
        let path = dir.join("hosts.toml");
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n");
        let store = TopologyStore::new(path.clone());
        let good = store.refresh();
        assert!(good.error.is_none());

        write(&path, "hub = \"a\"\n[host.a]\ncolour = \"red\"\n");
        bump_mtime(&path);
        let bad = store.refresh();
        assert!(bad.error.is_some(), "the on-disk file is now invalid");
        assert_eq!(bad.topo.unwrap().hub, "a", "the last GOOD topology is still served");
        assert!(!bad.changed, "a refused read never fires topology.changed");

        // Fix the file; the next refresh picks it back up normally.
        write(&path, "hub = \"a\"\n[host.a]\ndaemon = true\n\n[host.c]\n");
        bump_mtime(&path);
        let fixed = store.refresh();
        assert!(fixed.error.is_none());
        assert_eq!(fixed.topo.unwrap().hosts.len(), 2);
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sot-topology-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Coarse filesystem mtime resolution (notably some CI overlay/tmpfs
    /// combos) can otherwise leave two writes within one tick indistinct;
    /// the store's own fast path keys off `(mtime, len)`, and length alone
    /// already differs in every test above except this helper's callers,
    /// so this only exists to make the comparison robust regardless.
    fn bump_mtime(path: &Path) {
        let t = SystemTime::now() + std::time::Duration::from_secs(2);
        let _ = std::fs::File::open(path).and_then(|f| f.set_modified(t));
    }
}
