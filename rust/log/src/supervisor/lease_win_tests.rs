use super::*;

fn unique_name(tag: &str) -> String {
    // A real process's own pid plus a per-test tag: unique enough for
    // this test binary's own run without needing a full lease_name
    // round trip through a fake state-dir hash.
    format!(r"Local\sot-lease-test-{tag}-{:x}", std::process::id())
}

#[test]
fn lease_name_is_stable_and_scoped_by_hash_and_pid() {
    assert_eq!(lease_name("abc123", 0x2a), r"Local\sot-lease-abc123-2a");
}

/// Windows mutex OWNERSHIP is per-THREAD, not per-handle or per-process
/// (a thread that already owns a mutex re-acquires it recursively on a
/// further wait, returning `WAIT_OBJECT_0` rather than `WAIT_TIMEOUT`
/// -- see `LeaseCheck::is_broken`'s own table). `create` and `open`
/// called from the SAME thread would therefore make every check see a
/// recursive self-acquisition, never the real "still held by someone
/// else" case production always is (the checker is a SEPARATE
/// PROCESS). Every test below holds the lease on a DEDICATED thread
/// and checks from the test's own (different) thread, so a genuine
/// `WAIT_TIMEOUT` is what "not broken" actually observes.
fn hold_on_a_thread(name: String) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let lease = create(&name).unwrap();
        ready_tx.send(()).unwrap();
        let _ = release_rx.recv(); // park until told to finish
        drop(lease);
    });
    (ready_rx, release_tx, holder)
}

#[test]
fn a_freshly_created_lease_is_not_broken_while_held() {
    let name = unique_name("held");
    let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
    ready_rx.recv().unwrap();
    let check = open(&name).unwrap();
    assert!(!check.is_broken());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn dropping_the_owning_lease_marks_it_broken() {
    let name = unique_name("dropped");
    let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
    ready_rx.recv().unwrap();
    let check = open(&name).unwrap();
    assert!(!check.is_broken());
    release_tx.send(()).unwrap();
    // Abandonment is a property of the OWNING THREAD terminating, not
    // of the handle merely dropping -- join to be certain the thread
    // has actually exited before checking.
    holder.join().unwrap();
    assert!(check.is_broken());
}

#[test]
fn a_second_create_for_the_same_name_is_refused() {
    let name = unique_name("dup");
    let _first = create(&name).unwrap();
    let err = create(&name).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn opening_a_name_nobody_created_fails() {
    let name = unique_name("never-created");
    assert!(open(&name).is_err());
}

#[test]
fn multiple_independent_checks_all_observe_the_same_abandonment() {
    let name = unique_name("multi");
    let (ready_rx, release_tx, holder) = hold_on_a_thread(name.clone());
    ready_rx.recv().unwrap();
    let check_a = open(&name).unwrap();
    let check_b = open(&name).unwrap();
    assert!(!check_a.is_broken());
    assert!(!check_b.is_broken());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(check_a.is_broken());
    assert!(check_b.is_broken());
}
