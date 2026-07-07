// watcher.rs — notify-backed file watcher → broadcast of preview.changed
// events.
//
// One Watcher per backend. notify's recursive watcher reports raw OS events;
// a small async dispatcher debounces per-path bursts (editors typically save
// 2–4 events for one logical save), bumps the session ring once per logical
// change, and fans out to per-connection subscribers via a broadcast channel.
//
// Why bump the ring at all: clients that reconnect after a disconnect see the
// preview.changed entries via replay and re-fetch any cursored / pinned
// previews they care about. Without ring bumping, a reconnecting client
// silently keeps stale bytes.
//
// What we don't watch: high-churn build / VCS directories (`.git`, `target`,
// `node_modules`, …) — they fire constantly during a rust build / a git op
// and the preview pane has no use for either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher as NotifyWatcher};
use serde_json::json;
use tokio::sync::{broadcast, mpsc};

use crate::files_mode::FilesMode;
use crate::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Created,
    Removed,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Modified => "modified",
            ChangeKind::Created => "created",
            ChangeKind::Removed => "removed",
        }
    }
}

/// One deduplicated, session-bumped file change. Receivers turn this into a
/// `preview.changed` evt frame with the carried revision.
#[derive(Debug, Clone)]
pub struct PreviewChanged {
    pub revision: u64,
    pub path: PathBuf,
    pub node_id: Option<String>,
    pub kind: ChangeKind,
}

/// Live watcher. Holding the struct keeps the underlying notify::Watcher
/// alive (dropping it stops watching) and keeps the dispatcher task running.
pub struct Watcher {
    tx: broadcast::Sender<PreviewChanged>,
    // Behind Arc<Mutex> because the (potentially slow) recursive `watch()`
    // registration runs on a background thread — see `spawn`. Keeping a clone
    // here holds the notify::Watcher alive for the daemon's lifetime.
    _notify: Arc<Mutex<notify::RecommendedWatcher>>,
}

impl Watcher {
    pub fn spawn(root: &Path, session: Session, files_mode: Arc<FilesMode>) -> Result<Self> {
        let (broadcast_tx, _rx) = broadcast::channel::<PreviewChanged>(256);

        // notify's callback is sync; bridge to async via an unbounded mpsc.
        // Unbounded is fine — bursts are short, and the dispatcher empties
        // it as fast as it can read; if it can't, the backend has bigger
        // problems than memory pressure.
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<(PathBuf, ChangeKind)>();

        // Creating the watcher only opens the inotify fd and spawns notify's
        // own event thread — no tree walk — so this is fast and safe inline.
        let notify_watcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let Some(kind) = map_kind(&event.kind) else {
                        return;
                    };
                    for path in event.paths {
                        if should_skip(&path) {
                            continue;
                        }
                        let _ = raw_tx.send((path, kind));
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "notify watcher reported error");
                }
            },
        )
        .context("create notify watcher")?;
        let notify_watcher = Arc::new(Mutex::new(notify_watcher));

        // Register the recursive watch OFF the startup path. `watch(root,
        // Recursive)` walks the entire tree adding an inotify watch per
        // directory; on a slow or transiently-stalled NFS mount that walk can
        // block for minutes (seen in the wild: a `notify-rs` thread wedged in
        // `rpc_wait_bit_killable` while traversing `rust/target` on NFS). It
        // must NEVER hold up the caller: the daemon binds its protocol listener
        // *after* `spawn` returns, so a blocking walk here once left sotd
        // listening on the HTTP ports but never on the main socket — no
        // frontend could attach and the app looked dead. Doing it on a detached
        // thread decouples "can the FE connect" from "is the filesystem
        // responsive"; previews simply begin auto-refreshing once the
        // (background) registration lands.
        {
            let nw = notify_watcher.clone();
            let root = root.to_path_buf();
            std::thread::Builder::new()
                .name("watch-register".into())
                .spawn(move || match nw.lock() {
                    Ok(mut w) => match w.watch(&root, RecursiveMode::Recursive) {
                        Ok(()) => tracing::info!(root = ?root, "file watcher started"),
                        Err(e) => tracing::warn!(
                            error = %e, root = ?root,
                            "file watcher registration failed; previews will not auto-refresh on disk changes"
                        ),
                    },
                    Err(_) => {
                        tracing::warn!("file watcher mutex poisoned; skipping registration")
                    }
                })
                .context("spawn watch-register thread")?;
        }

        // Dispatcher: debounce per-path, bump the session ring, broadcast.
        let bus = broadcast_tx.clone();
        let fm = files_mode.clone();
        let sess = session.clone();
        tokio::spawn(async move {
            let mut last_emit: HashMap<PathBuf, Instant> = HashMap::new();
            let window = Duration::from_millis(200);
            while let Some((path, kind)) = raw_rx.recv().await {
                let now = Instant::now();
                if let Some(t) = last_emit.get(&path) {
                    if now.duration_since(*t) < window {
                        continue;
                    }
                }
                last_emit.insert(path.clone(), now);
                // Periodically prune the dedup map so a long-running daemon
                // touching many files doesn't grow it unboundedly. Cheap
                // walk: drop entries older than 10 * window.
                if last_emit.len() > 4096 {
                    let cutoff = Duration::from_millis(2_000);
                    last_emit.retain(|_, t| now.duration_since(*t) < cutoff);
                }

                let node_id = fm.path_to_node_id(&path);
                let payload = json!({
                    "path": path.to_string_lossy(),
                    "node_id": node_id,
                    "kind": kind.as_str(),
                });
                let revision = sess.bump("preview.changed", payload).await;

                // `send` only errors when there are zero subscribers. That's
                // fine — events still landed on the ring for the next client.
                let _ = bus.send(PreviewChanged {
                    revision,
                    path,
                    node_id,
                    kind,
                });
            }
        });

        Ok(Self {
            tx: broadcast_tx,
            _notify: notify_watcher,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PreviewChanged> {
        self.tx.subscribe()
    }
}

fn map_kind(k: &notify::EventKind) -> Option<ChangeKind> {
    use notify::EventKind::*;
    match k {
        Modify(_) => Some(ChangeKind::Modified),
        Create(_) => Some(ChangeKind::Created),
        Remove(_) => Some(ChangeKind::Removed),
        // Access (open / read / close-nowrite) is irrelevant for preview
        // re-fetch and is the highest-cardinality kind on Linux inotify.
        _ => None,
    }
}

/// Directories whose contents change constantly under normal use but never
/// belong in a preview pane. Skipping inside the notify callback is cheaper
/// than the unbounded-channel + dedup round-trip.
fn should_skip(path: &Path) -> bool {
    let mut prev: Option<&str> = None;
    for c in path.components() {
        if let std::path::Component::Normal(s) = c {
            if let Some(name) = s.to_str() {
                if matches!(
                    name,
                    ".git" | "target" | "node_modules" | ".julia" | "dist" | "build"
                ) {
                    return true;
                }
                // ADR 0022: `image.crop` writes land in `<root>/.sot/captures/`.
                // Skip them so a capture doesn't fire a spurious `preview.changed`
                // on top of its own `image.cropped` ring bump (matched as the
                // two-component path `.sot/captures`, so `.sot/settings.toml` and
                // an unrelated `captures/` elsewhere still come through).
                if name == "captures" && prev == Some(".sot") {
                    return true;
                }
                prev = Some(name);
            } else {
                prev = None;
            }
        } else {
            prev = None;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_skip_known_high_churn_dirs() {
        assert!(should_skip(Path::new("/a/.git/HEAD")));
        assert!(should_skip(Path::new("/a/target/debug/foo")));
        assert!(should_skip(Path::new("/a/sub/node_modules/x")));
        assert!(should_skip(Path::new("/a/.julia/registries/General.toml")));
    }

    #[test]
    fn should_skip_lets_normal_paths_through() {
        assert!(!should_skip(Path::new("/a/src/lib.rs")));
        assert!(!should_skip(Path::new("/a/.concept/types/Foo.md")));
        assert!(!should_skip(Path::new("/a/docs/readme.md")));
        // ADR 0022: crop output is skipped, but other .sot config + an
        // unrelated captures/ are not.
        assert!(should_skip(Path::new("/a/.sot/captures/img-roi-1.png")));
        assert!(!should_skip(Path::new("/a/.sot/settings.toml")));
        assert!(!should_skip(Path::new("/a/data/captures/run.csv")));
    }
}
