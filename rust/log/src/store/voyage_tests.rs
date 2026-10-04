//! The voyage store's tests: bootstrap, open and reopen, fence, lease, blob CAS, root pin and the Windows arms.

use super::*;
use super::super::support_tests::{intent_env, lc, lc_take};
use crate::envelope::Seq;
use crate::segment::Commit;

/// The concurrent-bootstrap race a shared staging pathname allowed
/// (review finding on the DACL unit): with one `.creating` path, attempt
/// B could delete attempt A's populated, flushed staging and substitute
/// an empty directory between A's flushes and A's rename — A then
/// publishes a voyage NOBODY flushed. Attempt-owned random staging names
/// dissolve the shared path entirely; `publish_noreplace` arbitrates.
/// This drives both attempts through a start barrier and requires:
/// exactly one winner, a verify-green published voyage (never an empty
/// or hybrid one), and zero staging residue once both attempts and the
/// winner's sweep are done.
#[test]
fn concurrent_bootstraps_publish_exactly_one_verifiable_voyage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voyr");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let results: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                VoyageStore::bootstrap(&root, "voyr", RetentionClass::Discard)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();

    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one bootstrap must win: {results:?}");
    // The published voyage is complete and internally consistent — the
    // race's failure mode was an EMPTY root published as success.
    crate::verify::verify_voyage(&root, "voyr").unwrap();
    let store = VoyageStore::open_for_writing(&root, "voyr").unwrap();
    drop(store);
    // Loser's guard plus winner's sweep leave no attempt residue.
    let residue: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("voyr.creating"))
        .collect();
    assert!(residue.is_empty(), "staging residue: {residue:?}");
}

#[test]
fn bootstrap_open_write_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy1");
    VoyageStore::bootstrap(&root, "voy1", RetentionClass::Discard).unwrap();
    assert!(root.join("seg").is_dir());
    assert!(root.join("blobs").join(".tmp").is_dir());
    // No staging residue of any attempt survives a successful bootstrap.
    let residue: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("voy1.creating"))
        .collect();
    assert!(residue.is_empty(), "staging residue left behind: {residue:?}");

    // Incarnation 1: epoch 1, write + seal one segment.
    {
        let mut store = VoyageStore::open_for_writing(&root, "voy1").unwrap();
        assert_eq!(store.epoch, 1);
        let mut w = store.open_segment(0).unwrap();
        for n in 1..=2 {
            w.append(&lc(1, n), Commit::Immediate).unwrap();
        }
        let d = w.seal(None).unwrap();
        store.advance_chain(d);
        // Leave a second segment OPEN with one frame (the survivor).
        let mut w2 = store.open_segment(0).unwrap();
        w2.append(&lc(1, 3), Commit::Immediate).unwrap();
        // Dropped without sealing: simulates writer death.
    }

    // Incarnation 2: epoch = 2, survivor sealed, chain continues.
    let mut store = VoyageStore::open_for_writing(&root, "voy1").unwrap();
    assert_eq!(store.epoch, 2);
    store.seal_survivor().unwrap();
    let mut w = store.open_segment(0).unwrap();
    assert_eq!(w.identity().segment_index, 2);
    w.append(&lc(2, 1), Commit::Immediate).unwrap();
    let d = w.seal(None).unwrap();
    store.advance_chain(d);

    // Verify the whole voyage.
    crate::verify::verify_voyage(&root, "voy1").unwrap();
}

#[test]
fn second_writer_is_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy2");
    VoyageStore::bootstrap(&root, "voy2", RetentionClass::Discard).unwrap();
    let _first = VoyageStore::open_for_writing(&root, "voy2").unwrap();
    let second = VoyageStore::open_for_writing(&root, "voy2");
    assert!(matches!(second, Err(Error::State(_))));
}

/// U1a (ADR 0041 Lifecycle "Discovery, and the two windows"): a broken
/// parent-death lease refuses with the dedicated typed error, and — the
/// ADR's own "releases the fence and exits without binding" — the fence
/// is provably free again immediately afterward: a fresh open right
/// after the broken-lease attempt must succeed, not see a lock the
/// failed attempt left held.
#[test]
fn lease_broken_releases_the_fence_and_exits_without_binding() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voylease");
    VoyageStore::bootstrap(&root, "voylease", RetentionClass::Discard).unwrap();

    let broken: &dyn Fn() -> bool = &|| true;
    let result = VoyageStore::open_for_writing_with_lease(&root, "voylease", Some(broken));
    assert!(matches!(result, Err(Error::LeaseBroken)), "{:?}", result.err());

    let reopened = VoyageStore::open_for_writing(&root, "voylease");
    assert!(
        reopened.is_ok(),
        "the fence must be released on a broken lease, not left held: {:?}",
        reopened.err()
    );
}

/// U1a Codex round-1, minor cluster: the EARLIER lease tests prove
/// error ORDERING (lease-broken beats history corruption) and
/// post-release availability, but neither actually proves the
/// callback runs WHILE the fence is held — it could, in principle, run
/// after `open_for_writing_with_lease` released it and still pass
/// those tests. This one proves it directly: the callback ITSELF
/// attempts a second concurrent open on the SAME root and must observe
/// lock contention, then reports the lease as intact so the OUTER open
/// still succeeds — proving both that the fence was genuinely held
/// at the moment of the call, and that the overall function still
/// works end to end once the callback returns.
#[test]
fn lease_callback_runs_while_the_fence_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voyleasefence");
    VoyageStore::bootstrap(&root, "voyleasefence", RetentionClass::Discard).unwrap();

    let root_for_probe = root.clone();
    let probe = move || {
        let second = VoyageStore::open_for_writing(&root_for_probe, "voyleasefence");
        assert!(
            matches!(second, Err(Error::State(_))),
            "the fence must still be held while the lease callback runs: {:?}",
            second.err()
        );
        false // lease intact -- let the outer open proceed
    };
    let store = VoyageStore::open_for_writing_with_lease(&root, "voyleasefence", Some(&probe));
    assert!(store.is_ok(), "{:?}", store.err());
}

/// A lease supplied but reporting itself intact is a pure no-op —
/// `open_for_writing_with_lease(..., Some(&|| false))` must behave
/// identically to `open_for_writing`'s own no-lease default.
#[test]
fn lease_intact_does_not_affect_an_ordinary_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voyleaseok");
    VoyageStore::bootstrap(&root, "voyleaseok", RetentionClass::Discard).unwrap();

    let intact: &dyn Fn() -> bool = &|| false;
    let store = VoyageStore::open_for_writing_with_lease(&root, "voyleaseok", Some(intact));
    assert!(store.is_ok(), "{:?}", store.err());
}

/// U1a: the fence and lease check run BEFORE history traversal, not
/// after — proven by a voyage whose sealed history is genuinely
/// corrupt (the same "bad forward-intent reference" fixture
/// `dedupe_fold_rejects_a_fact_naming_an_unresolvable_input` uses,
/// confirmed below to still fail the way that test expects for an
/// ordinary open). A BROKEN lease against this same store must refuse
/// with `Error::LeaseBroken`, never the history walk's own
/// `Error::Schema` — if the walk ran first, the corruption would win
/// the race and mask the lease check entirely.
#[test]
fn lease_check_precedes_history_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voyleasehist");
    VoyageStore::bootstrap(&root, "voyleasehist", RetentionClass::Discard).unwrap();
    {
        let mut store = VoyageStore::open_for_writing(&root, "voyleasehist").unwrap();
        let mut w = store.open_segment(0).unwrap();
        w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
        w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
        // A forward_intent fact naming a Seq that was never an input
        // frame -- the exact fixture the existing dedupe-fold test uses,
        // reused here so the corruption is provably genuine, not a
        // stand-in.
        w.append(&intent_env(1, 3, Seq { epoch: 1, n: 99 }), Commit::Immediate).unwrap();
        w.seal(None).unwrap();
    }

    // Confirm the corruption is real and reachable via an ordinary,
    // no-lease open -- otherwise the test below would prove nothing.
    let Err(err) = VoyageStore::open_for_writing(&root, "voyleasehist") else {
        panic!("expected the corrupt history to fail an ordinary open")
    };
    assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");

    // The SAME store, with a broken lease: the lease check must win
    // the race against the corrupt history walk.
    let broken: &dyn Fn() -> bool = &|| true;
    let result = VoyageStore::open_for_writing_with_lease(&root, "voyleasehist", Some(broken));
    assert!(
        matches!(result, Err(Error::LeaseBroken)),
        "the lease check must run before history traversal ever sees the corruption: {:?}",
        result.err()
    );
}

#[test]
fn blob_publish_idempotent_and_collision_loud() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy3");
    VoyageStore::bootstrap(&root, "voy3", RetentionClass::Discard).unwrap();
    let store = VoyageStore::open_for_writing(&root, "voy3").unwrap();
    let d1 = store.publish_blob(b"hello").unwrap();
    let d2 = store.publish_blob(b"hello").unwrap();
    assert_eq!(d1, d2);
    let path = root.join("blobs").join("sha256").join(&d1[0..2]).join(&d1);
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    // Forged content under the same name: loud on the next publish.
    std::fs::write(&path, b"evil!").unwrap();
    assert!(store.publish_blob(b"hello").is_err());
}

/// Part 2 finding, reproduced: bootstrap two real stores A and B, open
/// via a symlink pointed at A, retarget the symlink to B AFTER the fence
/// is taken, then keep writing. A writer that re-resolved the unresolved
/// alias at each later syscall would hold A's lock but write segments
/// into B; canonicalizing once in `open_for_writing` and storing the
/// result must keep every later operation on A regardless of where the
/// alias points now.
#[test]
#[cfg(unix)]
fn root_alias_cannot_escape_fence_after_open() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    VoyageStore::bootstrap(&a, "voy", RetentionClass::Discard).unwrap();
    VoyageStore::bootstrap(&b, "voy", RetentionClass::Discard).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&a, &alias).unwrap();

    let mut store = VoyageStore::open_for_writing(&alias, "voy").unwrap();

    // Retarget AFTER the fence is taken.
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&b, &alias).unwrap();

    let mut w = store.open_segment(0).unwrap();
    w.append(&lc(1, 1), Commit::Immediate).unwrap();
    w.seal(None).unwrap();

    let has_sealed = |dir: &std::path::Path| {
        std::fs::read_dir(dir.join("seg"))
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".sotseg"))
    };
    assert!(has_sealed(&a), "writer must operate on A, resolved at open time");
    assert!(!has_sealed(&b), "writer must NOT follow a post-open retarget into B");
}

/// ADR 0041 Codex round-1, Major 5 discharge: `prepare_root` +
/// `open_prepared` composed manually (the SAME composition
/// `open_for_writing` performs internally) reproduces an ordinary
/// open end to end — proving the split itself changes nothing
/// observable for a caller that performs both halves itself.
#[test]
fn prepare_root_then_open_prepared_reproduces_an_ordinary_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voysplit");
    VoyageStore::bootstrap(&root, "voysplit", RetentionClass::Discard).unwrap();

    let prepared = VoyageStore::prepare_root(&root).unwrap();
    assert_eq!(prepared.path(), std::fs::canonicalize(&root).unwrap());

    let mut store = VoyageStore::open_prepared(&prepared, "voysplit", None).unwrap();
    let mut w = store.open_segment(0).unwrap();
    w.append(&lc(1, 1), Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    crate::verify::verify_voyage(&root, "voysplit").unwrap();
}

/// ADR 0041 Codex round-2 discharge: `open_prepared`'s kernel-identity
/// check catches the resolved path ITSELF being retargeted (its
/// directory entry removed and replaced by a symlink) in the gap
/// between `prepare_root` and this process acquiring the fence — the
/// scenario the split's own safety reasoning depends on, distinct from
/// `root_alias_cannot_escape_fence_after_open` above (which retargets
/// an ALIAS TO the resolved path, never the resolved path itself, and
/// so never triggers this check at all).
#[test]
#[cfg(unix)]
fn open_prepared_refuses_when_the_canonical_root_itself_is_retargeted_before_the_fence() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    VoyageStore::bootstrap(&a, "voy", RetentionClass::Discard).unwrap();
    VoyageStore::bootstrap(&b, "voy", RetentionClass::Discard).unwrap();

    let prepared = VoyageStore::prepare_root(&a).unwrap();

    // Retarget the PREPARED PATH ITSELF (not merely an alias to it) --
    // A's own directory entry now resolves into B.
    std::fs::remove_dir_all(&a).unwrap();
    std::os::unix::fs::symlink(&b, &a).unwrap();

    let result = VoyageStore::open_prepared(&prepared, "voy", None);
    assert!(
        matches!(result, Err(Error::State(_))),
        "open_prepared must refuse when the prepared path's kernel identity no longer matches: {:?}",
        result.err()
    );
}

/// ADR 0041 Codex round-2 discharge: the reported gap, reproduced
/// exactly. A live reproducer proved the ORIGINAL check (re-canonicalize
/// and compare path TEXT) blind to this: bootstrap two stores, prepare
/// ONE of them, rename it aside, then rename the OTHER into its exact
/// former pathname -- `std::fs::canonicalize` on a plain directory
/// (no symlink anywhere in the chain) returns the identical string
/// before and after, so a path-text check sees nothing wrong and
/// `open_prepared` would silently open the WRONG STORE. The
/// kernel-identity check (`(st_dev, st_ino)`, read off the handle this
/// function itself opens) catches it: the directory now at that path
/// is a genuinely different object, identity differs, and the open is
/// refused.
#[test]
#[cfg(unix)]
fn open_prepared_refuses_a_same_pathname_directory_swap() {
    let dir = tempfile::tempdir().unwrap();
    let prepared_path = dir.path().join("a");
    let replacement_source = dir.path().join("b");
    let original_moved = dir.path().join("a-original");
    VoyageStore::bootstrap(&prepared_path, "voy", RetentionClass::Discard).unwrap();
    VoyageStore::bootstrap(&replacement_source, "voy", RetentionClass::Discard).unwrap();

    let prepared = VoyageStore::prepare_root(&prepared_path).unwrap();

    // The swap: move A aside, then move B into A's exact former
    // pathname. No symlink anywhere -- re-canonicalizing `prepared`'s
    // own path string would return the SAME string it always did.
    std::fs::rename(&prepared_path, &original_moved).unwrap();
    std::fs::rename(&replacement_source, &prepared_path).unwrap();

    let result = VoyageStore::open_prepared(&prepared, "voy", None);
    assert!(
        matches!(result, Err(Error::State(_))),
        "a same-pathname directory swap must be refused via kernel identity, not silently opened: {:?}",
        result.err()
    );
}

/// The happy path, unchanged: `prepare_root` then `open_prepared` with
/// NOTHING disturbed in between still succeeds -- the identity check
/// is a genuine verification, not a check that merely always fails or
/// always passes regardless of what actually happened.
#[test]
fn open_prepared_succeeds_when_nothing_is_disturbed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voyintact");
    VoyageStore::bootstrap(&root, "voyintact", RetentionClass::Discard).unwrap();

    let prepared = VoyageStore::prepare_root(&root).unwrap();
    let store = VoyageStore::open_prepared(&prepared, "voyintact", None);
    assert!(store.is_ok(), "{:?}", store.err());
}

/// ADR 0041 Codex round-2b: the reported gap, reproduced. A live repro
/// proved round-2's check-once-then-reopen-by-path design blind to an
/// ONGOING `RENAME_EXCHANGE` storm: it verified identity once, via a
/// transient open, then re-opened by PATH for the writer lock,
/// preflight, both flushes, and the segment directory -- any of which
/// a swap landing AFTER the check could still redirect. `seed_segments`
/// gives A and B a RELIABLE discriminator (`next_segment_index`, a
/// `pub` field on `VoyageStore`) -- NOT `retention_class`, which the
/// original repro used: `VoyageStore::bootstrap`'s own `retention`
/// parameter is not yet threaded into a fresh voyage's genesis segment
/// (a separate, pre-existing gap, unrelated to this fix -- see that
/// function's own `let _ = (voyage_id, retention)`), so EVERY freshly
/// opened store defaults to `RetentionClass::Archive` regardless of
/// what `bootstrap` was asked for, making that field unable to tell
/// the two stores apart at all.
///
/// Bounded runtime (a fixed WALL-CLOCK window, not a fixed iteration
/// count like the original repro's `0..20_000`): the storm keeps
/// racing for 1.5s. This value is empirically motivated, not a round
/// guess: reverting this fix and running this exact test against the
/// resulting (round-2) code, a 300ms window was NOT reliably long
/// enough to observe the wrong-store open at all on this development
/// machine, while 3s reliably was — 1.5s is a deliberately generous
/// middle ground so this test would actually CATCH a reintroduced
/// version of the bug, not merely fail to prove one that happens to
/// dodge a too-short window.
#[test]
#[cfg(target_os = "linux")]
fn open_prepared_refuses_under_a_sustained_rename_exchange_storm() {
    fn seed_segments(root: &Path, count: u64) {
        VoyageStore::bootstrap(root, "voy", RetentionClass::Discard).unwrap();
        let mut store = VoyageStore::open_for_writing(root, "voy").unwrap();
        for _ in 0..count {
            let w = store.open_segment(0).unwrap();
            let digest = w.seal(None).unwrap();
            store.advance_chain(digest);
        }
    }

    // `renameat2(RENAME_EXCHANGE)`: atomically swaps what `a` and `b`
    // each name, over and over -- the exact primitive the reported
    // repro used.
    fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let a = CString::new(a.as_os_str().as_bytes()).unwrap();
        let b = CString::new(b.as_os_str().as_bytes()).unwrap();
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    seed_segments(&a, 1);
    seed_segments(&b, 3);
    let prepared = VoyageStore::prepare_root(&a).unwrap();

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = std::sync::Arc::clone(&stop);
    let (thread_a, thread_b) = (a.clone(), b.clone());
    let swapper = std::thread::spawn(move || {
        while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = exchange(&thread_a, &thread_b);
        }
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    let mut attempts: u32 = 0;
    let mut refusals: u32 = 0;
    let mut wrong_store_opened = false;
    while std::time::Instant::now() < deadline {
        attempts += 1;
        match VoyageStore::open_prepared(&prepared, "voy", None) {
            Ok(store) if store.next_segment_index != 1 => {
                wrong_store_opened = true;
                break;
            }
            Ok(_) => {}
            Err(_) => refusals += 1,
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    swapper.join().unwrap();

    assert!(
        !wrong_store_opened,
        "open_prepared must never succeed with the replacement store's history"
    );
    assert!(attempts > 0, "the storm loop never even ran once");
    assert!(
        refusals > 0,
        "the storm never actually raced the check within {attempts} attempts -- \
         widen the window or the loop is not exercising the race at all"
    );
}

/// Part 3 finding: the CAS `dest.exists()` replay path must restate the
/// publication barrier over a blob this process didn't itself publish,
/// not merely fsync its shard directory. Holding the existing blob open
/// write-denied fails the renamed-target flush `finish_publication`
/// performs — while the CAS byte-compare read, which the old code also
/// performed, still succeeds — proving the flush is actually attempted.
#[test]
#[cfg(windows)]
fn cas_replay_reflushes_existing_blob_on_windows() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy4");
    VoyageStore::bootstrap(&root, "voy4", RetentionClass::Discard).unwrap();
    let store = VoyageStore::open_for_writing(&root, "voy4").unwrap();
    let d1 = store.publish_blob(b"hello").unwrap();
    let path = root.join("blobs").join("sha256").join(&d1[0..2]).join(&d1);

    // FILE_SHARE_READ, deny write: see `recovery.rs`'s `hold_with_share`
    // doc for why the hold must not block anything old code also does —
    // here that's only the CAS byte-compare read, so read-only sharing
    // is enough (unlike `recovering_alone_...` there, this path never
    // deletes `path`, so there is no deletion-denial trap to avoid).
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(&path)
        .unwrap();

    let e = store.publish_blob(b"hello").unwrap_err();
    assert!(matches!(e, Error::Io(_)), "{e}");
}

/// Independently derive this process's own token-user SID as a string —
/// deliberately NOT calling into `fsutil`'s private
/// `owner_protected_descriptor`, so a bug in THAT helper's SID lookup
/// could not also hide from these tests.
#[cfg(windows)]
fn current_user_sid_string() -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        assert_ne!(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token), 0);
        let mut needed: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        assert!(needed > 0, "GetTokenInformation sizing call returned zero length");
        let words = (needed as usize).div_ceil(8); // u64-backed: TOKEN_USER holds a pointer field
        let mut buf: Vec<u64> = vec![0u64; words];
        let buf_ptr = buf.as_mut_ptr().cast::<u8>();
        assert_ne!(
            GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed),
            0
        );
        let sid = (*buf_ptr.cast::<TOKEN_USER>()).User.Sid;
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        assert_ne!(ConvertSidToStringSidW(sid, &mut sid_str), 0);
        let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
        LocalFree(sid_str as _);
        CloseHandle(token);
        s
    }
}

/// Round-trip `path`'s security descriptor to SDDL text via
/// `GetNamedSecurityInfoW` + `ConvertSecurityDescriptorToStringSecurityDescriptorW`
/// — far simpler and less error-prone in a test that cannot be compiled
/// here than manually walking `ACL`/`ACE` binary structures with
/// `GetAce`. Requests DACL + PROTECTED_DACL info only (no owner/group/
/// sacl): the SDDL comes back as `D:P(...)` when protected, `D:(...)`
/// when not, with each ACE's inherit/inherited flags spelled out as
/// letters (`OICI` = object+container inherit, `ID` = inherited).
#[cfg(windows)]
fn security_descriptor_sddl(path: &std::path::Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    // DACL_SECURITY_INFORMATION alone: the PROTECTED_ flag is SET-ONLY
    // (Microsoft's SECURITY_INFORMATION table marks its query right
    // "not available") — the P in the returned SDDL comes from the
    // descriptor's own control field, not from asking for it.
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut psd,
        );
        assert_eq!(rc, 0, "GetNamedSecurityInfoW failed: {rc}");
        let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
        let mut sddl_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            psd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut sddl_ptr,
            &mut sddl_len,
        );
        assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
        let len = (0..).take_while(|&i| *sddl_ptr.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sddl_ptr, len));
        LocalFree(sddl_ptr as _);
        LocalFree(psd as _);
        s
    }
}

/// Round-trip an SDDL STRING through the converter pair (string -> SD ->
/// string) to the converter's own canonical form. The first CI run on a
/// real Windows machine taught why comparing raw SID strings to
/// converter output is wrong: `ConvertSecurityDescriptorToString...`
/// compresses well-known SIDs to their two-letter SDDL aliases — the
/// runner's built-in Administrator account (RID 500) came back as `LA`,
/// not `S-1-5-21-...-500` — while `ConvertSidToStringSidW` always emits
/// the raw form. Pushing the EXPECTED string through the same converter
/// makes both sides speak the converter's dialect, whatever account CI
/// happens to run as.
#[cfg(windows)]
fn canonical_sddl(sddl: &str) -> String {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let wide: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            ),
            0,
            "string->SD failed for {sddl}"
        );
        let mut out_ptr: *mut u16 = std::ptr::null_mut();
        let mut out_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            psd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut out_ptr,
            &mut out_len,
        );
        assert_ne!(ok, 0, "SD->string failed for {sddl}");
        let len = (0..).take_while(|&i| *out_ptr.add(i) != 0).count();
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(out_ptr, len));
        LocalFree(out_ptr as _);
        LocalFree(psd as _);
        out
    }
}

/// ADR 0041 DACL requirement, points 1 and 3 together: the published
/// root (this IS the post-rename state — bootstrap exposes no way to
/// inspect `.creating` before the rename, so this is simultaneously the
/// proof that the rename preserved the descriptor) carries a DACL that
/// is PRESENT and PROTECTED, with exactly one ACE granting the LIVE
/// token-user SID full access, marked object+container inheritable.
/// Fails against today's main: an un-DACL'd `create_dir_all` produces
/// an unprotected, inherited-from-parent descriptor with no such ACE.
#[test]
#[cfg(windows)]
fn bootstrap_voyage_root_gets_protected_dacl_for_token_user() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy5");
    VoyageStore::bootstrap(&root, "voy5", RetentionClass::Discard).unwrap();

    // FULL equality against the canonicalized expected form — presence,
    // protected bit, exactly one ACE, flags, access, and trustee in a
    // single assertion, robust to the converter's well-known-SID
    // aliasing (see `canonical_sddl`).
    let sid = current_user_sid_string();
    let expected = canonical_sddl(&format!("D:P(A;OICI;FA;;;{sid})"));
    assert_eq!(security_descriptor_sddl(&root), expected);
}

/// ADR 0041 DACL requirement, point 2: `seg/` — created inside the
/// staging root by `bootstrap`'s plain `create_dir_all`, with NO
/// security attributes of its own — carries an INHERITED ACE (`ID` =
/// INHERITED_ACE) for the same trustee, proving the tree propagates the
/// protection without any per-file work. A DIRECTORY child specifically
/// (rather than a leaf file the segment writer creates): Windows clears
/// the OI/CI propagation flags when materializing an inherited ACE onto
/// a FILE (they would have no meaning for something that can't have
/// children of its own), but a CONTAINER child keeps them — asserting
/// the exact `OICIID` flag combination is only reliable against another
/// container, so this checks `seg/` rather than the `.open` file inside
/// it. Fails against today's main for the same reason as the bootstrap
/// test above: there is no protected, inheritable ACE anywhere in the
/// tree to inherit FROM.
#[test]
#[cfg(windows)]
fn seg_dir_inherits_the_protected_dacl() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("voy6");
    VoyageStore::bootstrap(&root, "voy6", RetentionClass::Discard).unwrap();

    let sddl = security_descriptor_sddl(&root.join("seg"));
    // The expected inherited ACE is the ROOT's canonical ACE with the
    // INHERITED_ACE flag added: take the converter-canonical trustee
    // spelling (alias or raw, whatever this account canonicalizes to)
    // and splice ID into the flags we set — the flags are ours to know,
    // the trustee spelling is the converter's.
    let sid = current_user_sid_string();
    let canonical_root = canonical_sddl(&format!("D:P(A;OICI;FA;;;{sid})"));
    let ace = canonical_root
        .trim_start_matches("D:P")
        .replace("(A;OICI;", "(A;OICIID;");
    assert!(
        sddl.contains(&ace),
        "expected the inherited form {ace} of the root's ACE, got: {sddl}"
    );
}

// Unix is functionally unchanged by `create_dir_protected` — a plain
// `create_dir`, strict about AlreadyExists, which the attempt-owned
// random staging name makes unreachable in practice —
// `bootstrap_open_write_reopen` above is the proof that the whole
// bootstrap/write/reopen/verify path still holds on every unix/linux
// CI run.
