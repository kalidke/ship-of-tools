use super::*;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn absent_file_is_not_an_error() {
    let dir = tempdir();
    let store = TopologyStore::new(dir.path().join("hosts.toml"));
    let r = store.refresh();
    assert!(r.topo.is_none());
    assert!(r.hash.is_none());
    assert!(!r.changed);
    assert!(r.error.is_none());
}

#[test]
fn first_good_parse_is_not_reported_as_changed() {
    let dir = tempdir();
    let path = dir.path().join("hosts.toml");
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
    let path = dir.path().join("hosts.toml");
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
    let path = dir.path().join("hosts.toml");
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
    let path = dir.path().join("hosts.toml");
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

/// A fresh folder per call, named by tempfile's random suffix, so the
/// parallel tests never share one. The guard removes it when the test ends,
/// and also when an assertion panics first.
/// A FIFO planted where hosts.toml belongs ends in an error, keeping an earlier valid topology; the refresh runs on its
/// own thread so a read that waits fails the test instead of hanging the suite.
#[cfg(unix)]
#[test]
fn a_planted_fifo_is_refused_and_keeps_the_previous_topology() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempdir();
    let path = dir.path().join("hosts.toml");
    let store = std::sync::Arc::new(TopologyStore::new(path.clone()));
    write(&path, "hub = \"alpha\"\n[host.alpha]\ndaemon = true\n");
    assert!(store.refresh().topo.is_some());
    std::fs::remove_file(&path).unwrap();
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = store.clone();
    std::thread::spawn(move || {
        let _ = tx.send(s.refresh());
    });
    let r = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the refresh of a FIFO blocked");
    assert!(r.error.is_some());
    assert!(r.topo.is_some());
}

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sot-topology-store-test-")
        .tempdir()
        .unwrap()
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
