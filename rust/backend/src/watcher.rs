// watcher.rs — notify-backed file watcher → broadcast of preview.changed
// events.
//
// One Watcher PER WORKSPACE (since 2026-07-10; previously one per backend,
// which watched only the default workspace root — the documented KNOWN GAP
// where every other workspace's nav never live-refreshed). All watchers
// publish into ONE shared broadcast channel created by `server::run` (the
// repl_frame_tx fan-in pattern); events carry the owning workspace's slug so
// the FE can ignore other workspaces' traffic (node ids like
// `files:README.md` collide across repos). notify reports raw OS events; a
// small async dispatcher debounces per-path bursts (editors typically save 2–4
// events for one logical save), bumps the session ring once per logical change,
// and fans out to per-connection subscribers via a broadcast channel.
//
// Why bump the ring at all: clients that reconnect after a disconnect see the
// preview.changed entries via replay and re-fetch any cursored / pinned
// previews they care about. Without ring bumping, a reconnecting client
// silently keeps stale bytes.
//
// REGISTRATION (the 2026-07-11 fix): we do NOT hand notify one recursive
// `watch(root, Recursive)`. On Linux that registers one inotify watch per
// directory at REGISTRATION time — and `should_skip` only filters EVENTS, so a
// `--project-root $HOME` walk marched straight into a mounted NFS data share
// (`~/shares`) and exhausted `fs.inotify.max_user_watches` (~65k watches pinned
// for the daemon's life; every other inotify user starved). Instead a dedicated
// management thread (`run_watch_manager`) owns the notify::Watcher and walks the
// tree itself, adding a NON-recursive watch per KEPT directory:
//   - `should_skip` applied at registration (not just events);
//   - symlinked dirs never followed (loop / boundary-bypass hazard);
//   - never crossing a filesystem boundary (a dir whose st_dev differs from the
//     root's is skipped — this is what keeps the walk out of NFS `~/shares`);
//   - a watch-count budget (`watch_budget`) with graceful degrade past the cap;
//   - newly created directories get their (possibly already-populated) subtree
//     walked and watched, since NonRecursive doesn't auto-cover new subdirs.
// Registration mutations happen ONLY on that thread — never from notify's event
// callback (which would risk re-entrancy) — via a control channel the async
// dispatcher feeds directory add/remove hints into. Watcher events are treated
// as invalidation HINTS for preview refresh, not a complete change journal, so
// the small walk-vs-watch gaps are acceptable by design (reactive-over-eager).
//
// What we don't watch: high-churn build / VCS directories (`.git`, `target`,
// `node_modules`, …) — skipped at registration now, so they never consume a
// watch descriptor OR fire preview events during a rust build / git op.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    /// Slug of the workspace whose watcher saw the change. `None` only from
    /// a pre-multiwatch peer (compat); the FE treats that as default-ws.
    pub workspace_id: Option<String>,
}

/// Live watcher handle. The notify::Watcher itself is owned by the management
/// thread (`run_watch_manager`), not by this struct — so dropping the struct
/// signals that thread to release the watcher (and every inotify watch) and
/// exit. `Arc<AtomicBool>` keeps `Watcher` `Send + Sync` (it is held as
/// `Arc<Watcher>`, one per workspace).
pub struct Watcher {
    shutdown: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The management thread polls this (`recv_timeout`) and, when set,
        // releases the notify::Watcher — closing the inotify fd, dropping every
        // watch, and ending the dispatcher via the raw-event channel close.
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// Directory lifecycle hint from the async dispatcher to the management thread,
/// which owns the watcher and is the sole caller of `watch()`/`unwatch()`.
enum WatchCtrl {
    /// A path was created — if it is a directory on our filesystem, walk and
    /// watch its (possibly already-populated) subtree.
    MaybeAdd(PathBuf),
    /// A path was removed — prune it and anything under it from the budget
    /// accounting (inotify already freed the descriptors).
    MaybeRemove(PathBuf),
}

impl Watcher {
    /// Spawn a watcher for one workspace's root, publishing tagged events
    /// onto the SHARED `broadcast_tx` bus (created once in `server::run`).
    pub fn spawn(
        root: &Path,
        session: Session,
        files_mode: Arc<FilesMode>,
        broadcast_tx: broadcast::Sender<PreviewChanged>,
        workspace_slug: Option<String>,
    ) -> Result<Self> {

        // notify's callback is sync; bridge to async via an unbounded mpsc.
        // Unbounded is fine — bursts are short, and the dispatcher empties
        // it as fast as it can read; if it can't, the backend has bigger
        // problems than memory pressure.
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<(PathBuf, ChangeKind)>();

        // The notify callback is the ONLY thing on notify's internal event
        // thread. It just filters (`should_skip`) and forwards — it never calls
        // `watch()` (registration is the management thread's job, below), so
        // there is no re-entrancy against notify's own machinery. Creating the
        // watcher only opens the inotify fd + spawns notify's event thread (no
        // tree walk), so it is fast and safe inline.
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

        // Control channel: the async dispatcher forwards directory add/remove
        // hints to the management thread, which OWNS the watcher and is the sole
        // caller of `watch()`. std mpsc because that thread is sync.
        let (ctrl_tx, ctrl_rx) = std::sync::mpsc::channel::<WatchCtrl>();
        let shutdown = Arc::new(AtomicBool::new(false));

        // Management thread: owns the watcher, does the initial FILTERED walk OFF
        // the startup path (a slow / transiently-stalled NFS mount can wedge the
        // walk for minutes — seen in the wild — and it must NEVER hold up the
        // caller: the daemon binds its protocol listener AFTER `spawn` returns,
        // so a blocking walk here once left sotd listening on the HTTP ports but
        // never on the main socket, and the app looked dead), then serves control
        // requests for the watcher's lifetime. Previews begin auto-refreshing
        // once the background registration lands.
        {
            let root = root.to_path_buf();
            let shutdown = shutdown.clone();
            std::thread::Builder::new()
                .name("watch-manage".into())
                .spawn(move || run_watch_manager(notify_watcher, root, ctrl_rx, shutdown))
                .context("spawn watch-manage thread")?;
        }

        // Dispatcher: forward dir lifecycle hints to the manager, then debounce
        // per-path, bump the session ring, broadcast.
        let bus = broadcast_tx.clone();
        let fm = files_mode.clone();
        let sess = session.clone();
        let slug = workspace_slug;
        tokio::spawn(async move {
            let mut last_emit: HashMap<PathBuf, Instant> = HashMap::new();
            let window = Duration::from_millis(200);
            while let Some((path, kind)) = raw_rx.recv().await {
                // Watch-management hints go out BEFORE the debounce `continue`, so
                // a freshly created directory always reaches the manager even when
                // its preview event is debounced. The manager re-validates the
                // path (is it a dir on our filesystem, not a symlink?) itself.
                match kind {
                    ChangeKind::Created => {
                        let _ = ctrl_tx.send(WatchCtrl::MaybeAdd(path.clone()));
                    }
                    ChangeKind::Removed => {
                        let _ = ctrl_tx.send(WatchCtrl::MaybeRemove(path.clone()));
                    }
                    ChangeKind::Modified => {}
                }

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
                    "workspace_id": slug,
                });
                let revision = sess.bump("preview.changed", payload).await;

                // `send` only errors when there are zero subscribers. That's
                // fine — events still landed on the ring for the next client.
                let _ = bus.send(PreviewChanged {
                    revision,
                    path,
                    node_id,
                    kind,
                    workspace_id: slug.clone(),
                });
            }
        });

        Ok(Self { shutdown })
    }
}

/// Management thread body: owns the notify::Watcher for the workspace's lifetime,
/// registers the initial filtered/non-recursive/fs-bounded watch set, then serves
/// directory add/remove hints until the `Watcher` handle is dropped (`shutdown`)
/// or every control sender is gone. When it returns, `watcher` drops here —
/// closing the inotify fd, releasing every watch, and (via the raw-event channel
/// close) ending the async dispatcher.
fn run_watch_manager(
    mut watcher: notify::RecommendedWatcher,
    root: PathBuf,
    ctrl_rx: std::sync::mpsc::Receiver<WatchCtrl>,
    shutdown: Arc<AtomicBool>,
) {
    let cap = watch_budget();
    // The device the root lives on. Directories on a DIFFERENT device (a mount
    // point — NFS `~/shares`, a bind mount, an external disk) are not descended,
    // which is the primary guard against the watch-table exhaustion. `0` (the
    // failure fallback, and the non-unix value) matches nothing to a real dev, so
    // the boundary check simply never fires — safe, just unbounded by device.
    let root_dev = std::fs::symlink_metadata(&root)
        .map(|m| device_of(&m))
        .unwrap_or(0);
    let mut watched: HashSet<PathBuf> = HashSet::new();
    register_subtree(&mut watcher, &root, root_dev, &mut watched, cap);
    tracing::info!(
        root = ?root, watched = watched.len(), cap, root_dev,
        "file watcher registered (filtered, non-recursive, filesystem-bounded)"
    );

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match ctrl_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(WatchCtrl::MaybeAdd(p)) => {
                // Walk the new path's subtree — a plain mkdir adds one dir, but a
                // `git clone` / `tar x` / atomic rename-into-place lands a whole
                // populated tree, and NonRecursive won't cover it otherwise.
                register_subtree(&mut watcher, &p, root_dev, &mut watched, cap);
            }
            Ok(WatchCtrl::MaybeRemove(p)) => {
                // inotify auto-frees a watch when its dir is deleted; we only keep
                // the budget accounting honest. Prune the path AND anything under
                // it (an `rm -rf` frees the whole subtree's descriptors at once).
                let before = watched.len();
                watched.retain(|w| !w.starts_with(&p));
                let pruned = before - watched.len();
                if pruned > 0 {
                    tracing::debug!(removed = ?p, pruned, "watcher: pruned watches under removed path");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    tracing::debug!(root = ?root, "file watcher manager exiting; releasing watches");
}

/// One-time warning that we stopped short of full coverage at the budget cap.
static BUDGET_WARNED: std::sync::Once = std::sync::Once::new();

/// Depth-first, iterative walk from `start`, adding a NON-recursive inotify watch
/// on every directory that passes the filters, up to the remaining `cap`.
///
/// Filters (all at REGISTRATION, which is the fix — the event-time `should_skip`
/// alone never refunds a watch descriptor):
///   - `should_skip` — build/VCS dirs we never preview;
///   - symlinked dirs — never followed (loop + filesystem-boundary-bypass hazard);
///   - a different filesystem than `root_dev` — skipped, so a `$HOME` walk never
///     marches into a mounted NFS data share (the exact 2026-07-11 trigger);
///   - the watch budget — past it we stop and log once, degrading to "deeper
///     subtrees don't auto-refresh" rather than exhausting the inotify table.
fn register_subtree(
    watcher: &mut notify::RecommendedWatcher,
    start: &Path,
    root_dev: u64,
    watched: &mut HashSet<PathBuf>,
    cap: usize,
) {
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if watched.len() >= cap {
            BUDGET_WARNED.call_once(|| {
                tracing::warn!(
                    cap,
                    "watcher: reached the watch budget — deeper subtrees won't auto-refresh previews (nav still refreshes reactively on navigate). Tune SOT_WATCH_BUDGET or fs.inotify.max_user_watches to cover more."
                );
            });
            return;
        }
        if should_skip(&dir) {
            continue;
        }
        // symlink_metadata does NOT follow the final component, so a symlinked
        // directory is caught here (is_symlink) and skipped instead of followed.
        let md = match std::fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(_) => continue, // vanished between enqueue and now — skip
        };
        if md.file_type().is_symlink() || !md.is_dir() {
            continue;
        }
        let dir_dev = device_of(&md);
        if root_dev != 0 && dir_dev != root_dev {
            tracing::debug!(dir = ?dir, dev = dir_dev, root_dev, "watcher: skipping cross-filesystem subtree (e.g. NFS mount)");
            continue;
        }
        if !watched.insert(dir.clone()) {
            continue; // already watching (duplicate create / racing walk)
        }
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            watched.remove(&dir);
            if is_watch_limit(&e) {
                tracing::warn!(
                    watched = watched.len(),
                    error = %e,
                    "watcher: hit the OS inotify watch/instance limit — stopping registration here. Deeper/other subtrees won't auto-refresh previews until restart (reactive nav refresh still works); raise fs.inotify.max_user_watches to cover more."
                );
                return;
            }
            // A transient per-dir failure (racing removal, permissions) — skip it,
            // keep walking the rest.
            tracing::debug!(dir = ?dir, error = %e, "watcher: watch() failed for dir; skipping");
            continue;
        }
        // Enqueue child directories. `read_dir` + `entry.file_type()` uses d_type
        // on Linux (no extra stat), and a symlink-to-dir reports is_symlink (not
        // is_dir), so it is naturally not descended here.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        stack.push(entry.path());
                    }
                }
            }
        }
    }
}

/// Does this notify error mean "the OS inotify budget is full"? notify maps the
/// inotify `ENOSPC` (watch/instance limit) to `MaxFilesWatch`, but be defensive
/// and also treat a raw `ENOSPC`/`EMFILE`/`ENFILE` `Io` error the same way.
fn is_watch_limit(e: &notify::Error) -> bool {
    if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) {
        return true;
    }
    if let notify::ErrorKind::Io(io) = &e.kind {
        // ENOSPC=28 (inotify watch/instance limit), EMFILE=24, ENFILE=23.
        return matches!(io.raw_os_error(), Some(28) | Some(24) | Some(23));
    }
    false
}

/// Cap on directories a single workspace watcher will register. A conservative
/// absolute default, optionally lowered to a quarter of the OS-wide inotify limit
/// so we leave headroom for other inotify users (editors, other daemons) rather
/// than claiming the whole table. Override with `SOT_WATCH_BUDGET` for unusual
/// deployments (huge local monorepos on a box with a raised sysctl).
fn watch_budget() -> usize {
    const DEFAULT_CAP: usize = 8192;
    if let Ok(v) = std::env::var("SOT_WATCH_BUDGET") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    match read_max_user_watches() {
        // Quarter of the table, but never below a usable floor and never above
        // the conservative default.
        Some(m) => DEFAULT_CAP.min((m / 4).max(256)),
        None => DEFAULT_CAP,
    }
}

/// The OS-wide inotify watch limit (`fs.inotify.max_user_watches`), if readable.
fn read_max_user_watches() -> Option<usize> {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The filesystem device a metadata refers to (`st_dev`). Used to detect mount
/// boundaries so the walk never descends into a mounted share. Unix-only; on
/// other platforms it returns `0`, which disables the boundary check (matches
/// nothing to a real device).
#[cfg(unix)]
fn device_of(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.dev()
}

#[cfg(not(unix))]
fn device_of(_md: &std::fs::Metadata) -> u64 {
    0
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

    // A unique scratch dir for a filesystem test, following the pattern the other
    // backend tests use (no tempfile dep). Caller removes it.
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sot-watch-{}-{}", tag, std::process::id()))
    }

    fn noop_watcher() -> notify::RecommendedWatcher {
        notify::recommended_watcher(|_res: notify::Result<notify::Event>| {}).unwrap()
    }

    #[test]
    fn register_subtree_skips_build_vcs_and_symlink_dirs() {
        use std::fs;
        let base = scratch("filter");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/inner")).unwrap();
        fs::create_dir_all(base.join("docs")).unwrap();
        fs::create_dir_all(base.join(".git/objects")).unwrap(); // skip (VCS)
        fs::create_dir_all(base.join("target/debug")).unwrap(); // skip (build)
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(base.join("src"), base.join("link_to_src"));

        let mut w = noop_watcher();
        let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
        let mut watched: HashSet<PathBuf> = HashSet::new();
        register_subtree(&mut w, &base, root_dev, &mut watched, 10_000);

        // Kept: the real project dirs.
        assert!(watched.contains(&base));
        assert!(watched.contains(&base.join("src")));
        assert!(watched.contains(&base.join("src/inner")));
        assert!(watched.contains(&base.join("docs")));
        // Skipped at REGISTRATION: build/VCS subtrees consume no watch descriptor.
        assert!(!watched.iter().any(|p| p.starts_with(base.join(".git"))));
        assert!(!watched.iter().any(|p| p.starts_with(base.join("target"))));
        // A symlinked directory is never followed.
        #[cfg(unix)]
        assert!(!watched.contains(&base.join("link_to_src")));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn register_subtree_respects_budget_cap() {
        use std::fs;
        let base = scratch("cap");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        for i in 0..5 {
            fs::create_dir_all(base.join(format!("d{i}"))).unwrap();
        }
        // 6 dirs available (base + 5); a cap of 3 must bound registration.
        let mut w = noop_watcher();
        let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
        let mut watched: HashSet<PathBuf> = HashSet::new();
        register_subtree(&mut w, &base, root_dev, &mut watched, 3);
        assert_eq!(watched.len(), 3, "the budget cap must bound the watch count");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn watch_budget_honors_env_override() {
        std::env::set_var("SOT_WATCH_BUDGET", "1234");
        assert_eq!(watch_budget(), 1234);
        std::env::remove_var("SOT_WATCH_BUDGET");
        // Without the override it must be a sane positive cap.
        assert!(watch_budget() >= 256);
    }
}
