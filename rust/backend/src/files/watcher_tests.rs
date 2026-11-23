use super::*;

#[test]
fn should_skip_known_high_churn_dirs() {
    assert!(should_skip(Path::new("/a/.git/HEAD"), &[]));
    assert!(should_skip(Path::new("/a/target/debug/foo"), &[]));
    assert!(should_skip(Path::new("/a/sub/node_modules/x"), &[]));
    assert!(should_skip(Path::new("/a/.julia/registries/General.toml"), &[]));
}

#[test]
fn should_skip_lets_normal_paths_through() {
    assert!(!should_skip(Path::new("/a/src/lib.rs"), &[]));
    assert!(!should_skip(Path::new("/a/.concept/types/Foo.md"), &[]));
    assert!(!should_skip(Path::new("/a/docs/readme.md"), &[]));
    // ADR 0022: crop output is skipped, but other .sot config + an
    // unrelated captures/ are not.
    assert!(should_skip(Path::new("/a/.sot/captures/img-roi-1.png"), &[]));
    assert!(!should_skip(Path::new("/a/.sot/settings.toml"), &[]));
    assert!(!should_skip(Path::new("/a/data/captures/run.csv"), &[]));
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
    register_subtree(&mut w, &base, root_dev, &[], &mut watched, 10_000);

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
    register_subtree(&mut w, &base, root_dev, &[], &mut watched, 3);
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

#[test]
fn should_skip_excludes_our_own_state_tree() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // The root itself and everything under it: the updater creates its
    // staging directory here and renames it into place, and a watch open
    // on that directory is what makes the rename fail on Windows.
    assert!(should_skip(Path::new("/h/.local/state/sot"), &ours));
    assert!(should_skip(
        Path::new("/h/.local/state/sot/updates/tmp-v0.6.6-target/bin"),
        &ours
    ));
    // Without the exclusion it is an ordinary directory — which is exactly
    // how the watcher came to hold a handle on the stager's temp dir.
    assert!(!should_skip(
        Path::new("/h/.local/state/sot/updates/tmp-v0.6.6-target/bin"),
        &[]
    ));
}

#[test]
fn should_skip_keeps_a_sibling_whose_name_shares_the_prefix() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // Component-wise, never a string prefix — a user directory that merely
    // begins with the same characters is a real project and stays watched.
    assert!(!should_skip(
        Path::new("/h/.local/state/sot-notes/a.md"),
        &ours
    ));
    assert!(!should_skip(Path::new("/h/.local/state/sotalike"), &ours));
}

#[test]
fn should_skip_still_passes_ordinary_project_paths_when_excluding() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // The exclusion must not silently turn file watching off.
    assert!(!should_skip(Path::new("/h/projects/app/src/lib.rs"), &ours));
    assert!(!should_skip(Path::new("/h/README.md"), &ours));
}

#[test]
fn register_subtree_never_watches_our_own_tree() {
    use std::fs;
    let base = scratch("self");
    let _ = fs::remove_dir_all(&base);
    // `sot` is ours; `sot-notes` merely starts with the same characters.
    fs::create_dir_all(base.join("sot/updates/tmp-stage")).unwrap();
    fs::create_dir_all(base.join("sot-notes/deep")).unwrap();
    fs::create_dir_all(base.join("src")).unwrap();

    let ours = vec![base.join("sot")];
    let mut w = noop_watcher();
    let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
    let mut watched: HashSet<PathBuf> = HashSet::new();
    register_subtree(&mut w, &base, root_dev, &ours, &mut watched, 10_000);

    assert!(
        !watched.iter().any(|p| p.starts_with(base.join("sot"))),
        "no watch descriptor may be spent anywhere under our own tree"
    );
    assert!(watched.contains(&base.join("sot-notes")));
    assert!(watched.contains(&base.join("sot-notes/deep")));
    assert!(watched.contains(&base.join("src")));

    let _ = fs::remove_dir_all(&base);
}

/// Native smoke: retry the write until registration and OS delivery succeed. No two writes have to fit one burst.
#[tokio::test]
async fn a_real_watcher_delivers_a_write_in_its_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let fm = Arc::new(FilesMode::new(dir.path().to_owned()).unwrap());
    let file = fm.root_path().join("README.md");
    let (tx, mut rx) = broadcast::channel(256);
    let _watcher = Watcher::spawn(fm.root_path(), fm.clone(), tx, Some("ws".into())).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            std::fs::write(&file, "new").unwrap();
            if let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                if event.path == file && event.kind != ChangeKind::Removed {
                    assert_eq!(event.node_id.as_deref(), Some("files:README.md"));
                    assert_eq!(event.workspace_id.as_deref(), Some("ws"));
                    break;
                }
            }
        }
    }).await.expect("no native file change delivered in 20 seconds");
}

/// A burst is told by what it held, never by the order the system reported it in: FSEvents reports one record's
/// flags create first, then removal, then write, whatever happened, so a replaced file on macOS arrives as create,
/// removal, write.
#[test]
fn a_burst_is_told_by_what_it_held_in_any_order() {
    use ChangeKind::{Created, Modified, Removed};
    let orders: [&[ChangeKind]; 9] = [
        &[Modified, Modified],
        &[Removed, Created, Modified],
        &[Created, Removed, Modified],
        &[Modified, Created],
        &[Created, Modified, Removed],
        &[Removed],
        &[Modified, Removed],
        &[Removed, Modified],
        &[Created],
    ];
    for burst in orders {
        let told = burst[1..].iter().fold(burst[0], |told, next| told.merge(*next));
        let expected = if burst.contains(&Created) {
            Created
        } else if burst.contains(&Removed) {
            Removed
        } else {
            Modified
        };
        assert_eq!(told, expected, "burst {burst:?}");
    }
}

/// Drive the dispatcher's own raw input and poll its future by hand. Advancing the test clock cannot run it.
struct Dispatcher {
    _dir: tempfile::TempDir,
    ended: bool,
    hints: std::sync::mpsc::Receiver<WatchCtrl>,
    root: PathBuf,
    raw: Option<mpsc::UnboundedSender<(PathBuf, ChangeKind)>>,
    task: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    bus: broadcast::Receiver<PreviewChanged>,
}

impl Dispatcher {
    fn new() -> Self {
        Self::with_probe(|path| async move { std::fs::symlink_metadata(path).map(drop) })
    }

    fn with_probe<F: std::future::Future<Output = io::Result<()>> + 'static>(
        probe: impl Fn(PathBuf) -> F + 'static,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fm = Arc::new(FilesMode::new(dir.path().to_owned()).unwrap());
        let root = fm.root_path().to_owned();
        let (raw, rx) = mpsc::unbounded_channel();
        let (bus, events) = broadcast::channel(512);
        let (ctrl, hints) = std::sync::mpsc::channel();
        let task = Box::pin(dispatch_changes(rx, ctrl, fm, bus, Some("row".into()), probe));
        Self { _dir: dir, hints, ended: false, root, raw: Some(raw), task, bus: events }
    }

    fn send(&self, name: &str, kind: ChangeKind) {
        self.raw.as_ref().unwrap().send((self.root.join(name), kind)).unwrap();
    }

    async fn poll(&mut self) -> bool {
        if !self.ended { self.ended = futures_util::poll!(&mut self.task).is_ready(); }
        self.ended
    }

    fn take_events(&mut self) -> Vec<(String, ChangeKind)> {
        std::iter::from_fn(|| self.bus.try_recv().ok()).map(|event| {
            assert_eq!(event.workspace_id.as_deref(), Some("row"));
            (event.node_id.unwrap(), event.kind)
        }).collect()
    }

    async fn collect(&mut self, count: usize) -> Vec<(String, ChangeKind)> {
        let mut events = self.take_events();
        for _ in 0..512 {
            if events.len() >= count || self.poll().await { break; }
            tokio::task::yield_now().await;
            events.extend(self.take_events());
        }
        events.extend(self.take_events());
        events
    }

    async fn finish(&mut self) -> Vec<(String, ChangeKind)> {
        self.raw.take();
        for _ in 0..1024 {
            if self.poll().await { return self.take_events(); }
            tokio::task::yield_now().await;
        }
        panic!("watcher did not end at the current virtual instant");
    }
}

#[tokio::test(start_paused = true)]
async fn raw_bursts_close_once_at_their_deadline_using_the_path_then() {
    use ChangeKind::*;
    let mut d = Dispatcher::new();
    for (name, events, exists) in [
        ("write", vec![Modified, Modified], true), ("rename", vec![Modified], true),
        ("unlink-create", vec![Removed, Created], true), ("unlink-rename", vec![Removed, Modified], true),
        ("create-delete", vec![Created, Removed], false), ("missing", vec![Modified], false),
    ] {
        if exists { std::fs::write(d.root.join(name), "contents").unwrap(); }
        for kind in events { d.send(name, kind); }
    }
    d.poll().await;
    tokio::time::advance(BURST - Duration::from_millis(1)).await;
    d.poll().await;
    assert!(matches!(d.bus.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    tokio::time::advance(Duration::from_millis(1)).await;
    let expected = [("write", Modified), ("rename", Modified), ("unlink-create", Created), ("unlink-rename", Created),
                    ("create-delete", Removed), ("missing", Removed)];
    assert_eq!(d.collect(expected.len()).await,
        expected.map(|(n, k)| (format!("files:{n}"), k)), "one event per path, in deadline order");
    d.send("write", Modified);
    d.poll().await;
    tokio::time::advance(BURST).await;
    assert_eq!(d.collect(1).await, [("files:write".into(), Modified)]);
    assert!(d.finish().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_input_ready_after_the_deadline_opens_another_burst() {
    let mut d = Dispatcher::new();
    let name = "tie";
    std::fs::write(d.root.join(name), "exists").unwrap();
    d.send(name, ChangeKind::Created);
    d.poll().await;
    tokio::time::advance(BURST).await;
    d.send(name, ChangeKind::Removed); // Both ready, with raw first in the biased select.
    assert_eq!(d.collect(1).await, [("files:tie".into(), ChangeKind::Created)]);
    std::fs::remove_file(d.root.join(name)).unwrap();
    tokio::time::advance(BURST).await;
    assert_eq!(d.collect(1).await, [("files:tie".into(), ChangeKind::Removed)]);
    assert!(d.finish().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn closing_the_input_tells_every_open_path_in_order() {
    let mut d = Dispatcher::new();
    for i in 0..64 { d.send(&i.to_string(), ChangeKind::Removed); }
    d.poll().await;
    let expected = (0..64).map(|i| (format!("files:{i}"), ChangeKind::Removed)).collect::<Vec<_>>();
    assert_eq!(d.finish().await, expected);
}

thread_local! { pub(super) static DEADLINES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[tokio::test(start_paused = true)]
async fn each_input_checks_a_constant_number_of_deadlines_and_due_sends_yield() {
    let mut d = Dispatcher::new();
    DEADLINES_READ.set(0);
    for i in 0..128 {
        d.send(&i.to_string(), ChangeKind::Removed);
        d.poll().await;
    }
    let examined = DEADLINES_READ.get();
    tokio::time::advance(BURST).await;
    d.poll().await;
    let sent_without_yielding = d.bus.len();
    println!("128 inputs examined {examined} deadlines; one poll sent {sent_without_yielding}");
    assert!(examined <= 4 * 128 && sent_without_yielding <= 1,
        "128 inputs examined {examined} deadlines; one poll sent {sent_without_yielding}");
}

#[tokio::test]
async fn an_unreadable_path_still_tells_clients_to_fetch() {
    let mut d = Dispatcher::new();
    d.send("invalid\0write", ChangeKind::Modified);
    d.send("invalid\0unlink", ChangeKind::Removed);
    assert_eq!(d.finish().await, [("files:invalid\0write".into(), ChangeKind::Modified),
                                  ("files:invalid\0unlink".into(), ChangeKind::Created)]);
}

/// Each lookup records its start and waits for its own answer. Polling the dispatcher never advances time.
fn held_probes() -> (Dispatcher, mpsc::UnboundedReceiver<(PathBuf, tokio::sync::oneshot::Sender<io::Result<()>>)>) {
    let (requests, rx) = mpsc::unbounded_channel();
    (Dispatcher::with_probe(move |path| {
        let requests = requests.clone();
        async move {
            let (answer, wait) = tokio::sync::oneshot::channel();
            requests.send((path, answer)).unwrap();
            wait.await.unwrap()
        }
    }), rx)
}

#[tokio::test(start_paused = true)]
async fn a_held_lookup_leaves_intake_and_the_next_bursts_clock_running() {
    use ChangeKind::*;
    let (mut d, mut probes) = held_probes();
    d.send("first", Modified);
    d.poll().await;
    tokio::time::advance(BURST - Duration::from_millis(1)).await;
    d.poll().await;
    assert!(probes.try_recv().is_err() && d.bus.is_empty(), "the first probe began early");
    tokio::time::advance(Duration::from_millis(1)).await;
    d.poll().await;
    let (_, first) = probes.try_recv().unwrap();
    d.send("dir", Created);
    d.poll().await;
    assert!(matches!(d.hints.try_recv(), Ok(WatchCtrl::MaybeAdd(p)) if p == d.root.join("dir")));
    tokio::time::advance(Duration::from_millis(150)).await;
    d.send("dir", Removed);
    d.poll().await;
    assert!(matches!(d.hints.try_recv(), Ok(WatchCtrl::MaybeRemove(p)) if p == d.root.join("dir")));
    first.send(Ok(())).unwrap();
    assert_eq!(d.collect(1).await, [("files:first".into(), Modified)]);
    tokio::time::advance(Duration::from_millis(49)).await;
    d.poll().await;
    assert!(probes.try_recv().is_err() && d.bus.is_empty(), "a probe or publication preceded the original deadline");
    tokio::time::advance(Duration::from_millis(1)).await;
    d.poll().await;
    let (path, next) = probes.try_recv().expect("the staggered removal postponed the directory's close");
    assert_eq!(path, d.root.join("dir"));
    next.send(Err(io::ErrorKind::NotFound.into())).unwrap();
    assert_eq!(d.finish().await, [("files:dir".into(), Removed)]);
}

#[tokio::test(start_paused = true)]
async fn eof_preserves_order_for_two_bursts_of_a_path_while_the_first_probe_waits() {
    use ChangeKind::*;
    let (mut d, mut probes) = held_probes();
    d.send("a", Created);
    d.poll().await;
    d.hints.try_recv().unwrap();
    tokio::time::advance(BURST).await;
    d.poll().await;
    let (_, first) = probes.try_recv().unwrap();
    d.send("a", Removed);
    d.poll().await;
    assert!(matches!(d.hints.try_recv(), Ok(WatchCtrl::MaybeRemove(_))));
    d.raw.take();
    d.poll().await;
    assert!(d.bus.is_empty());
    first.send(Ok(())).unwrap();
    assert_eq!(d.collect(1).await, [("files:a".into(), Created)]);
    d.poll().await;
    let (_, second) = probes.try_recv().unwrap();
    second.send(Err(io::ErrorKind::NotFound.into())).unwrap();
    assert_eq!(d.finish().await, [("files:a".into(), Removed)]);
}

#[tokio::test(start_paused = true)]
async fn saturation_and_eof_bound_closed_bursts_and_preserve_the_oldest_path_order() {
    use ChangeKind::*;
    let (mut d, mut probes) = held_probes();
    d.send("repeat", Removed);
    d.poll().await;
    tokio::time::advance(BURST).await;
    d.poll().await;
    let (_, held) = probes.try_recv().unwrap();
    // 64 lookup slots including the active one. The repeated path overflows at EOF.
    for i in 1..64 { d.send(&i.to_string(), Modified); }
    d.send("repeat", Modified);
    d.raw.take();
    for _ in 0..160 { d.poll().await; }
    let mut told = d.take_events();
    assert_eq!(told, [("files:repeat".into(), Created)], "saturation must make room in publication order");
    assert!(held.send(Err(io::ErrorKind::NotFound.into())).is_err(), "a displaced lookup still had a recipient");
    // All other lookups finish with their budget intact. The saturated successor must never request one.
    for i in 1..64 {
        d.poll().await;
        let (path, answer) = probes.try_recv().expect("a lookup below capacity should run");
        assert_eq!(path, d.root.join(i.to_string()));
        answer.send(Ok(())).unwrap();
        told.extend(d.collect(1).await);
    }
    told.extend(d.finish().await);
    assert!(probes.try_recv().is_err(), "the saturated close started a lookup");
    let mut expected = vec![("files:repeat".into(), Created)];
    expected.extend((1..64).map(|i| (format!("files:{i}"), Modified)));
    expected.push(("files:repeat".into(), Modified));
    assert_eq!(told, expected);
}

#[tokio::test(start_paused = true)]
async fn retirement_exhausts_one_lookup_budget_for_the_whole_queue() {
    use ChangeKind::*;
    let (mut d, mut probes) = held_probes();
    for i in 0..64 { d.send(&i.to_string(), Removed); }
    d.raw.take();
    for _ in 0..130 { d.poll().await; }
    let (_, _held) = probes.try_recv().unwrap();
    tokio::time::advance(BURST - Duration::from_millis(1)).await;
    d.poll().await;
    assert!(d.bus.is_empty() && !d.ended);
    tokio::time::advance(Duration::from_millis(1)).await;
    let expected = (0..64).map(|i| (format!("files:{i}"), Created)).collect::<Vec<_>>();
    assert_eq!(d.finish().await, expected);
    assert!(probes.try_recv().is_err(), "expired probes were started");
}

#[tokio::test(start_paused = true)]
async fn late_handoff_does_not_extend_the_lookup_budget() {
    let (mut d, mut probes) = held_probes();
    d.send("late", ChangeKind::Removed);
    d.poll().await;
    tokio::time::advance(2 * BURST).await;
    assert_eq!(d.collect(1).await, [("files:late".into(), ChangeKind::Created)]);
    assert!(probes.try_recv().is_err(), "an expired close started a lookup");
    assert!(d.finish().await.is_empty());
}

#[tokio::test]
async fn a_descendant_of_a_regular_file_is_absent() {
    let mut d = Dispatcher::new();
    std::fs::write(d.root.join("parent"), "regular file").unwrap();
    d.send("parent/old-child", ChangeKind::Created);
    assert_eq!(d.finish().await, [("files:parent/old-child".into(), ChangeKind::Removed)]);
}

#[tokio::test(start_paused = true)]
async fn raw_intake_runs_between_overflow_publications() {
    let (mut d, _probes) = held_probes();
    for i in 0..256 {
        let name = i.to_string();
        d.send(&name, ChangeKind::Created);
        tokio::task::yield_now().await;
        d.poll().await;
        assert!(matches!(d.hints.try_recv(), Ok(WatchCtrl::MaybeAdd(p)) if p == d.root.join(name)));
    }
    tokio::time::advance(BURST).await;
    // Let the first overflow yield. A directory then arrives behind many other due paths.
    for _ in 0..256 {
        d.poll().await;
        if !d.bus.is_empty() { break; }
    }
    assert!(!d.bus.is_empty());
    d.send("new-dir", ChangeKind::Created);
    for _ in 0..2 {
        tokio::task::yield_now().await;
        d.poll().await;
    }
    assert!(matches!(d.hints.try_recv(), Ok(WatchCtrl::MaybeAdd(p)) if p == d.root.join("new-dir")),
        "overflow publication held up a new directory hint");
    d.raw.take();
    for _ in 0..512 {
        tokio::task::yield_now().await;
        d.poll().await;
    }
    tokio::time::advance(BURST).await;
    assert_eq!(d.finish().await.len(), 257);
}
