//! Tests of the registry lock: the take, the reclaim and the proofs, against holders in other processes.

use super::*;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set, the next post-settle re-read fails once (the review's EMFILE/EIO).
pub(super) static FAIL_REREAD: AtomicBool = AtomicBool::new(false);

/// Set, the next step's first read of the lock fails once (its holder
/// released, and a live writer linked it before any existence test).
pub(super) static FAIL_FIRST_READ: AtomicBool = AtomicBool::new(false);

/// Set, the record written into the lock just after the next step removes it.
pub(super) static AFTER_STEP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Set, the lock is removed at the end of the next settle, before its re-read.
pub(super) static GONE_IN_SETTLE: AtomicBool = AtomicBool::new(false);

/// Set, the lock is removed after the next failed retake, before its fresh read.
pub(super) static GONE_AFTER_RETAKE: AtomicBool = AtomicBool::new(false);

/// Held by every test here: the records read `SOT_SELF_HOST`, which other tests in this binary set.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// This process's record for a lock in the temp folder, where every scratch home here sits.
fn now_me() -> Me {
    Me::now(&std::env::temp_dir().join(".registry.lock"))
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "sot-registry-lock-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

/// A child leading its own process group. `end`, or a drop on any path (a failed assertion's unwind included),
/// kills the whole group and reaps the child.
struct Group(Option<Child>);

impl Group {
    fn end(&mut self) {
        if let Some(mut c) = self.0.take() {
            unsafe { libc::kill(-(c.id() as libc::pid_t), libc::SIGKILL) };
            let _ = c.wait();
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.end();
    }
}

/// Another process of this machine holding the lock in `home`: a `sleep` whose record, as its own take writes it,
/// is the lock's one line.
fn live_holder(home: &Path) -> Group {
    let c = Command::new("sleep").arg("30").stdout(Stdio::null()).process_group(0).spawn().unwrap();
    let record = record_for(c.id());
    fs::write(home.join(".registry.lock"), format!("{record}\n")).unwrap();
    Group(Some(c))
}

/// A holder that died inside its critical section: its record, written while it ran, is the lock's.
fn dead_holder(home: &Path) {
    let mut g = live_holder(home);
    g.end();
    assert!(home.join(".registry.lock").is_file(), "the dead holder left its lock");
}

/// A record names its process for the judge: alive while it runs, dead once it has gone.
#[test]
fn a_record_is_judged_alive_then_dead_with_its_process() {
    let _env = env_lock();
    let home = scratch("judge");
    let mut child = live_holder(&home);
    let id = fs::read_to_string(home.join(".registry.lock")).unwrap().trim().to_string();
    let me = now_me();
    assert!(matches!(judge(&id, &me).0, Verdict::Alive));
    child.end();
    assert!(matches!(judge(&id, &me).0, Verdict::Dead));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_lock_whose_holder_was_killed_is_reclaimed() {
    let _env = env_lock();
    let home = scratch("reclaim");
    dead_holder(&home);
    let dead = fs::read_to_string(home.join(".registry.lock")).unwrap();
    let lock = home.join(".registry.lock");
    let held = acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50)).expect("reclaimed");
    let marker = format!("{}.reclaim.{}", lock.display(), dead.trim().replace(':', "."));
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", now_me().id), "the marker is mine");
    drop(held);
    assert!(!lock.exists(), "released");
    assert!(Path::new(&marker).exists(), "markers are kept");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_live_holder_is_named_and_never_reclaimed() {
    let _env = env_lock();
    let home = scratch("live");
    let mut child = live_holder(&home);
    let lock = home.join(".registry.lock");
    let before = fs::read(&lock).unwrap();
    let err = acquire_with(&lock, Duration::from_millis(200), Duration::from_millis(50)).err().expect("FAILED");
    let f: Vec<String> = String::from_utf8_lossy(&before).trim().split(':').map(str::to_string).collect();
    let want = format!("is held by {} pid {} start {} (", f[0], f[4], f[5]);
    assert!(err.contains(&want), "{err}");
    assert!(err.ends_with(&format!(
        "): it is running. If it is dead, run any comm command on {}.",
        f[0]
    )), "{err}");
    assert_eq!(fs::read(&lock).unwrap(), before, "the lock is untouched");
    child.end();
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_panic_inside_the_lock_gives_up_the_file_and_the_turn() {
    let _env = env_lock();
    let home = scratch("panic");
    let lock = home.join(".registry.lock");
    let l = lock.clone();
    let r = std::thread::spawn(move || {
        let _held = acquire_with(&l, Duration::from_secs(1), Duration::from_millis(50)).expect("took it");
        panic!("inside the lock");
    })
    .join();
    assert!(r.is_err(), "the holder panicked");
    assert!(!lock.exists(), "the file went with the unwind");
    drop(acquire_with(&lock, Duration::ZERO, Duration::from_millis(50)).expect("the turn went with the unwind"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn waiting_for_another_threads_turn_counts_inside_the_bound() {
    let _env = env_lock();
    let home = scratch("turn");
    let lock = home.join(".registry.lock");
    let held = acquire_with(&lock, Duration::from_secs(1), Duration::from_millis(50)).expect("took it");
    let l = lock.clone();
    let t0 = Instant::now();
    let err = std::thread::spawn(move || acquire_with(&l, Duration::from_millis(200), Duration::from_millis(50)).err())
        .join()
        .unwrap()
        .expect("FAILED while another thread holds the turn");
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(200), "{waited:?}");
    assert!(err.contains("another thread of this daemon held its turn past the bound"), "{err}");
    drop(held);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_marker_this_process_abandoned_is_adopted_by_its_next_writer() {
    let _env = env_lock();
    let home = scratch("adopt");
    dead_holder(&home);
    let lock = home.join(".registry.lock");
    let dead = fs::read_to_string(&lock).unwrap();
    let marker = PathBuf::from(format!("{}.reclaim.{}", lock.display(), dead.trim().replace(':', ".")));
    FAIL_REREAD.store(true, Ordering::SeqCst);
    let l = lock.clone();
    let first = std::thread::spawn(move || acquire_with(&l, Duration::ZERO, Duration::from_millis(50)).map(drop))
        .join()
        .unwrap();
    assert!(first.is_err(), "the writer whose re-read failed gave up");
    assert_eq!(fs::read_to_string(&lock).unwrap(), dead, "the dead holder's lock stayed");
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", now_me().id), "the marker names this process");
    let l = lock.clone();
    let second = std::thread::spawn(move || acquire_with(&l, Duration::from_secs(2), Duration::from_millis(50)).map(drop))
        .join()
        .unwrap();
    assert!(second.is_ok(), "the next writer adopted the marker: {second:?}");
    assert!(!lock.exists(), "released");
    assert!(acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50)).is_ok(), "a later writer is not wedged");
    let markers = fs::read_dir(&home).unwrap().filter(|e| {
        e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".registry.lock.reclaim.")
    });
    assert_eq!(markers.count(), 1, "the one marker was adopted, not passed");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_lock_naming_this_daemon_is_reclaimed_by_its_next_writer() {
    let _env = env_lock();
    let home = scratch("self");
    let lock = home.join(".registry.lock");
    let me = format!("{}\n", now_me().id);
    fs::write(&lock, &me).unwrap();
    let held = acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50));
    assert!(held.is_ok(), "a lock a thread of this daemon left does not wedge it: {:?}", held.err());
    drop(held);
    assert!(!lock.exists(), "released");
    let marker = marker_for(&lock, me.trim());
    assert!(!marker.exists(), "its own marker went with the lock");
    // As a re-read that failed leaves it: the next writer adopts the marker.
    fs::write(&marker, &me).unwrap();
    fs::write(&lock, &me).unwrap();
    assert!(acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50)).is_ok(), "and again, past a kept marker");
    assert!(!marker.exists(), "which went with the lock too");
    let _ = fs::remove_dir_all(&home);
}

/// This daemon's own leftover lock, removed during its reclaim's settle
/// (by hand, or its own late unlink): its marker goes all the same
/// (review SF1).
#[test]
fn its_own_marker_goes_when_its_lock_goes_during_the_settle() {
    let _env = env_lock();
    let home = scratch("gone");
    let lock = home.join(".registry.lock");
    let me = format!("{}\n", now_me().id);
    fs::write(&lock, &me).unwrap();
    GONE_IN_SETTLE.store(true, Ordering::SeqCst);
    let held = acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50));
    assert!(held.is_ok(), "{:?}", held.err());
    drop(held);
    assert!(!marker_for(&lock, me.trim()).exists(), "its own marker went with the lock");
    let _ = fs::remove_dir_all(&home);
}

/// A zero wait whose retake fails, and whose fresh read then finds the
/// lock gone: a free lock, never one to remove by hand (review note 3).
#[test]
fn a_lock_free_at_the_deadline_is_never_to_be_removed_by_hand() {
    let _env = env_lock();
    let home = scratch("free");
    let lock = home.join(".registry.lock");
    dead_holder(&home);
    *AFTER_STEP.lock().unwrap() = Some("elsewhere:-:-:-:4242:-\n".into());
    GONE_AFTER_RETAKE.store(true, Ordering::SeqCst);
    let err = acquire_with(&lock, Duration::ZERO, Duration::from_millis(50)).err().expect("FAILED");
    assert!(!err.contains("by hand") && err.contains(RETAKEN), "{err}");
    assert!(!lock.exists(), "the lock is free");
    let _ = fs::remove_dir_all(&home);
}

/// The step after a blocked one finds the lock gone; the text must not keep
/// the earlier step's by-hand flag, and must say a holder it last read WAS held.
fn gone_text(holder: Option<&str>, by_hand: bool) -> String {
    let mut b = Blocked { holder: holder.map(Into::into), who: None, why: "it is alive".into(), by_hand, gone: false };
    b.released();
    b.fail_text(Path::new("/nonexistent/.registry.lock"))
}

#[test]
fn a_lock_gone_at_the_last_step_is_not_by_hand() {
    let err = gone_text(None, true);
    assert!(!err.contains("by hand"), "{err}");
}

#[test]
fn a_lock_released_since_the_last_read_was_held_not_is_held() {
    let err = gone_text(Some("elsewhere:-:-:-:4242:-"), true);
    assert!(
        err.ends_with("was held by elsewhere pid 4242 start - when last read (it is alive); it may have been released since. Retry."),
        "{err}"
    );
    assert!(!err.contains("is held") && !err.contains("comm-registry-lock-clear") && !err.contains("by hand"), "{err}");
}

/// A comm home whose mount proves no fresh read proves no death: this process's
/// record there has no proof fields, so a dead holder there is named by hand
/// (review F1). /proc here, and on macOS an automounted folder.
#[test]
fn a_comm_home_the_vouch_refuses_gives_a_record_with_no_proof() {
    let lock = Path::new(if cfg!(target_os = "macos") { "/System/Volumes/Data/home/.registry.lock" } else { "/proc/self/.registry.lock" });
    assert!(vouch(lock).is_err(), "the mount is refused");
    let me = Me::now(lock);
    assert!(me.mine.is_none(), "{}", me.id);
    assert_eq!(me.id, format!("{}:-:-:-:{}:-", host_field(), std::process::id()));
}

/// A holder that cannot be proved dead here, on another machine or in another
/// pid namespace, is named with its host, and, if it is dead, by hand where
/// nothing runs there again or a comm command there cannot prove a death
/// (reviews F2, R3-B1); a holder the step did not judge never by hand (SF2).
#[test]
fn an_unprovable_holder_names_its_host_and_by_hand_if_it_will_not_run_again() {
    let text = |why: &str| {
        let far = Some("far:m:b:1:4242:7".to_string());
        Blocked { holder: far.clone(), who: far, why: why.into(), by_hand: false, gone: false }
            .fail_text(Path::new("/nonexistent/.registry.lock"))
    };
    let p = "/nonexistent/.registry.lock";
    assert!(text("it is on another machine").ends_with(&format!(
        "If it is dead: run any comm command on far, or, if far will not run again or a comm command there says it cannot prove a death, remove {p} by hand."
    )));
    assert!(text("it is in another pid namespace").ends_with(&format!(
        "If it is dead: run any comm command on far in its pid namespace, or, if nothing will run there again or a comm command there says it cannot prove a death, remove {p} by hand."
    )));
    assert!(text("it is running").ends_with("): it is running. If it is dead, run any comm command on far."));
    let unjudged = Blocked { holder: Some("mac:-:-:-:4343:-".into()), who: None, why: "it was taken again during the reclaim".into(), by_hand: false, gone: false };
    let err = unjudged.fail_text(Path::new(p));
    assert!(err.ends_with("it was taken again during the reclaim. If it is dead, run any comm command on mac."), "{err}");
}

/// A record with no proof fields no box can prove dead, so its FAILED line
/// says to remove the lock by hand, never to run a comm command on its host.
#[test]
fn a_holder_with_no_proof_fields_is_removed_by_hand() {
    let mac = Some("mac:-:-:-:4343:-".to_string());
    let b = Blocked { holder: mac.clone(), who: mac, why: "it is on another machine".into(), by_hand: false, gone: false };
    let err = b.fail_text(Path::new("/nonexistent/.registry.lock"));
    assert!(err.ends_with(
        "(unknown old): it is on another machine. No box can prove mac pid 4343 dead (its record has no proof fields): if it is, remove /nonexistent/.registry.lock by hand."
    ), "{err}");
}

/// A live holder's record the step cannot read (a permission or I/O
/// error) is released, never by hand, and its text says it was not read.
#[test]
fn an_unreadable_live_record_is_released_not_by_hand() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    let home = scratch("unread");
    let mut child = live_holder(&home);
    let lock = home.join(".registry.lock");
    let before = fs::read(&lock).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
    let r = step(&lock, &now_me(), Duration::from_millis(50), Instant::now() + Duration::from_secs(2));
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(r, Err(None)), "a record not read is released");
    assert_eq!(fs::read(&lock).unwrap(), before, "the live holder's lock is untouched");
    let mut b = Blocked { holder: None, who: None, why: "it was released just now".into(), by_hand: true, gone: false };
    b.released();
    let err = b.fail_text(&lock);
    assert!(!err.contains("by hand") && err.ends_with("Retry.") && err.contains("could not be read"), "{err}");
    child.end();
    let _ = fs::remove_dir_all(&home);
}

/// The review's interleaving: the first read fails (its holder released)
/// and a live writer links the lock before any existence test.
#[test]
fn a_failed_first_read_with_a_live_writer_in_the_lock_is_released() {
    let _env = env_lock();
    let home = scratch("first");
    let mut child = live_holder(&home);
    let lock = home.join(".registry.lock");
    let before = fs::read(&lock).unwrap();
    FAIL_FIRST_READ.store(true, Ordering::SeqCst);
    let r = step(&lock, &now_me(), Duration::from_millis(50), Instant::now() + Duration::from_secs(2));
    assert!(matches!(r, Err(None)), "a failed first read is released, never by hand");
    assert_eq!(fs::read(&lock).unwrap(), before, "the live writer's lock is untouched");
    child.end();
    let _ = fs::remove_dir_all(&home);
}

/// What was read whole still decides by hand: `why` of a by-hand step.
fn by_hand_why(tag: &str, make: impl FnOnce(&Path)) -> String {
    let _env = env_lock();
    let home = scratch(tag);
    let lock = home.join(".registry.lock");
    make(&lock);
    let r = step(&lock, &now_me(), Duration::from_millis(50), Instant::now() + Duration::from_secs(2));
    let _ = fs::remove_dir_all(&home);
    match r {
        Err(Some(b)) if b.by_hand => b.why,
        _ => panic!("{tag}: not by hand"),
    }
}

#[test]
fn an_empty_record_read_whole_is_by_hand() {
    assert_eq!(by_hand_why("empty", |l| fs::write(l, "").unwrap()), "its record names no holder (empty)");
}

#[test]
fn an_older_versions_directory_is_by_hand() {
    assert_eq!(by_hand_why("olddir", |l| fs::create_dir(l).unwrap()), "held by an older version that records no holder");
}

#[test]
fn an_id_without_proof_is_never_mine() {
    let _env = env_lock();
    let me = now_me();
    assert!(me.is_me(&me.id), "this process, with its proof");
    let bare = Me { id: format!("{}:-:-:-:{}:-", host_field(), std::process::id()), mine: None };
    assert!(!bare.is_me(&bare.id), "the same host name and pid, with no proof");
}

/// A dead holder D whose marker `reclaim.<D>` names D (a daemon that died
/// during its own reclaim): the next writer stops at the marker within its
/// bound, and says to remove the lock by hand. Hermetic t14 runs the clear.
#[test]
fn a_marker_naming_its_own_dead_holder_stops_every_walk() {
    let _env = env_lock();
    let home = scratch("cycle");
    dead_holder(&home);
    let lock = home.join(".registry.lock");
    let dead = fs::read_to_string(&lock).unwrap();
    let marker = marker_for(&lock, dead.trim());
    fs::write(&marker, &dead).unwrap();
    let err = acquire_with(&lock, Duration::from_millis(300), Duration::from_millis(50)).err().expect("FAILED");
    assert!(err.contains(&format!("its reclaim marker {} names {}", marker.display(), dead.trim())), "{err}");
    assert!(err.ends_with(&format!("remove {} by hand and retry.", lock.display())), "{err}");
    assert_eq!(fs::read_to_string(&lock).unwrap(), dead, "the lock is untouched");
    assert_eq!(fs::read_to_string(&marker).unwrap(), dead, "and its marker");
    let _ = fs::remove_dir_all(&home);
}

/// Hermetic t12's twin: a zero wait whose retake after the step finds a
/// second dead holder D2 fails at once, and never steps again (review SF3).
#[test]
fn a_zero_wait_whose_retake_fails_makes_one_step() {
    let _env = env_lock();
    let home = scratch("retake");
    let lock = home.join(".registry.lock");
    dead_holder(&home);
    let d2 = fs::read_to_string(&lock).unwrap();
    fs::remove_file(&lock).unwrap();
    dead_holder(&home);
    let d1 = fs::read_to_string(&lock).unwrap();
    *AFTER_STEP.lock().unwrap() = Some(d2.clone());
    let err = acquire_with(&lock, Duration::ZERO, Duration::from_millis(200)).err().expect("FAILED");
    let f: Vec<&str> = d2.trim().split(':').collect();
    assert!(err.contains(&format!("is held by {} pid {} start {} (", f[0], f[4], f[5])) && err.contains(RETAKEN), "{err}");
    assert_eq!(fs::read_to_string(&lock).unwrap(), d2, "D2's lock is untouched");
    assert!(marker_for(&lock, d1.trim()).exists() && !marker_for(&lock, d2.trim()).exists(), "exactly one step");
    let _ = fs::remove_dir_all(&home);
}

/// The two-host suite's reader: prints the fresh read of
/// `SOT_TEST_LOCK_PATH`, or `UNREADABLE`.
#[test]
#[ignore]
fn print_fresh() {
    let path = PathBuf::from(std::env::var_os("SOT_TEST_LOCK_PATH").expect("SOT_TEST_LOCK_PATH"));
    println!("FRESH {}", fresh(&path, true).unwrap_or_else(|_| "UNREADABLE".into()));
}
