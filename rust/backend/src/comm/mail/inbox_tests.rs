//! Tests of the inbox append, its lock record and its routing (see inbox.rs).

use super::*;

fn read_lines(p: &Path) -> Vec<serde_json::Value> {
    let s = std::fs::read_to_string(p).unwrap();
    assert!(s.ends_with('\n'), "not LF-terminated: {s:?}");
    s.lines()
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect()
}

// T1 — the line shape, one line per call, a newline in the text stays
// inside the one record.
#[test]
fn file_frame_writes_the_bridge_line_shape_one_per_call() {
    let d = tempfile::tempdir().unwrap();
    let w = Duration::from_secs(1);
    file_frame(d.path(), "a", "h", false, "one", "2026-01-02T03:04:05Z", w, "local t").unwrap();
    file_frame(d.path(), "a", "h", false, "two\nlines", "2026-01-02T03:04:06Z", w, "local t").unwrap();
    let raw = std::fs::read_to_string(d.path().join("h.jsonl")).unwrap();
    assert_eq!(
        raw.lines().next().unwrap(),
        r#"{"from":"a","to":"h","repo":"daemon","msg":"one","ts":"2026-01-02T03:04:05Z"}"#
    );
    let lines = read_lines(&d.path().join("h.jsonl"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["msg"], "two\nlines");
    assert!(d.path().join("h.lock").exists(), "the sidecar is the lock");
}

// T3 — two writers in one process, 200 lines each, through the lock.
#[test]
fn two_threads_give_400_whole_lines() {
    let d = tempfile::tempdir().unwrap();
    two_writers(d.path());
}

pub(super) fn two_writers(dir: &Path) {
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|w| {
            let dir = dir.to_path_buf();
            std::thread::spawn(move || {
                for i in 0..200 {
                    let text = format!("{w}-{i} {}", "x".repeat(64));
                    file_frame(&dir, w, "t3", false, &text, "t", inbox_lock_wait(), "local t").unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let lines = read_lines(&dir.join("t3.jsonl"));
    assert_eq!(lines.len(), 400);
    let mut seen: Vec<String> = lines
        .iter()
        .map(|l| l["msg"].as_str().unwrap().split(' ').next().unwrap().to_string())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 400, "a line was lost or doubled");
}

fn inbox_lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap().lines().map(str::to_string).collect()
}

// S2 — an unterminated tail is CUT back to the last newline, the new line
// goes in whole and last.
#[test]
fn a_torn_tail_is_cut_before_the_new_line() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("h.jsonl");
    std::fs::write(&f, "{\"a\":1}\n{\"from\":\"died\",").unwrap();
    file_frame(d.path(), "a", "h", false, "whole", "t", Duration::from_secs(1), "local t").unwrap();
    let lines = read_lines(&f);
    assert_eq!(lines.len(), 2, "{:?}", std::fs::read_to_string(&f));
    assert_eq!(lines[0]["a"], 1);
    assert_eq!(lines[1]["msg"], "whole");
}

// S-a — NULs after a client crash are cut like any tail: a file of only
// NULs is cut to empty and then holds the new line; no newline at all is
// cut to 0.
#[test]
fn a_nul_tail_and_a_newline_free_file_are_cut() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("h.jsonl");
    std::fs::write(&f, [0u8; 5000]).unwrap();
    file_frame(d.path(), "a", "h", false, "alone", "t", Duration::from_secs(1), "local t").unwrap();
    assert_eq!(read_lines(&f).len(), 1);
    std::fs::write(&f, "{\"a\":1}\n\0\0\0").unwrap();
    file_frame(d.path(), "a", "h", false, "after", "t", Duration::from_secs(1), "local t").unwrap();
    assert_eq!(read_lines(&f).len(), 2);
    std::fs::write(&f, "no newline at all").unwrap();
    file_frame(d.path(), "a", "h", false, "only", "t", Duration::from_secs(1), "local t").unwrap();
    let lines = read_lines(&f);
    assert_eq!((lines.len(), lines[0]["msg"].as_str()), (1, Some("only")));
}

// A tail longer than one block is still cut at the last newline.
#[test]
fn a_tail_longer_than_a_block_is_cut_at_the_last_newline() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("h.jsonl");
    std::fs::write(&f, format!("{{\"a\":1}}\n{}", "x".repeat(10_000))).unwrap();
    file_frame(d.path(), "a", "h", false, "whole", "t", Duration::from_secs(1), "local t").unwrap();
    assert_eq!(inbox_lines(&f).len(), 2);
    assert_eq!(read_lines(&f)[0]["a"], 1);
}

#[cfg(target_os = "linux")]
fn scripts_lib() -> String {
    format!("{}/../../comm/core/scripts/comm-lib.sh", env!("CARGO_MANIFEST_DIR"))
}

#[cfg(target_os = "linux")]
fn shell(home: &Path, path: &str, script: &str) -> std::process::Output {
    std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("source {}; {script}", scripts_lib()))
        .env("SOT_COMM_HOME", home)
        .env("PATH", path)
        .output()
        .unwrap()
}

// The stubbed fsync failure, Rust arm: `sync` starts a reader, waits until
// it is in flight (counted, or blocked behind the exclusive lock) and
// fails, so `append_line` cuts the in-flight line back. The reader is the
// scripts' own (comm-lib.sh), so the guards under test are the real ones.
// A comm home whose folder record matches this disk lets the locked reader
// take the shared lock; a PATH with no flock(1) leaves the other one
// unlocked. The reader's 30 s wait gives the writer's release, after the poll of
// `until_a_waiter_blocks_on` detects it, headroom under load.
#[cfg(target_os = "linux")]
fn failing_sync_with_reader(
    home: &Path,
    path: &str,
    reader: &'static str,
    reader_blocks: bool,
) -> (String, String) {
    let inbox = home.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let id = shell(home, &std::env::var("PATH").unwrap(), r#"sot_inbox_lock_identity "$INBOX_DIR""#);
    std::fs::write(home.join("inbox-lock-manager"), &id.stdout).unwrap();
    file_frame(&inbox, "a", "h", false, "one", "t", Duration::from_secs(5), "local t").unwrap();
    let child = std::cell::RefCell::new(None);
    let sync = |_f: &File| -> std::io::Result<()> {
        let mut c = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("source {}; {reader}", scripts_lib()))
            .env("SOT_COMM_HOME", home)
            .env("PATH", path)
            .env("SOT_INBOX_READ_WAIT_SECS", "30")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if reader_blocks {
            until_a_waiter_blocks_on(&inbox.join("h.lock"));
        } else {
            c.wait().unwrap();
        }
        *child.borrow_mut() = Some(c);
        Err(std::io::Error::other("stubbed fsync failure"))
    };
    let r = file_frame_with(&inbox, "a", "h", false, "inflight", "t", Duration::from_secs(5), "local t", sync);
    assert!(r.unwrap_err().contains("stubbed fsync failure"));
    assert_eq!(read_lines(&inbox.join("h.jsonl")).len(), 1, "the in-flight line was not cut back");
    let out = child.into_inner().unwrap().wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

// The kernel's word that a reader or filer is blocked behind the lock:
// /proc/locks lists a waiter as a line with `->` on the lock's inode. The
// 20 s guard keeps margin over the 10 s filing wait in `held_then_freed`.
#[cfg(target_os = "linux")]
fn until_a_waiter_blocks_on(lock: &Path) {
    use std::os::unix::fs::MetadataExt;
    let needle = format!(":{} ", std::fs::metadata(lock).unwrap().ino());
    let give_up = Instant::now() + Duration::from_secs(20);
    loop {
        let locks = std::fs::read_to_string("/proc/locks").unwrap();
        if locks.lines().any(|l| l.contains("->") && l.contains(&needle)) {
            return;
        }
        assert!(Instant::now() < give_up, "no waiter blocked on {lock:?} within 20 s");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// (a) The reader that waits on the shared lock counts nothing new after the
// cut-back.
#[cfg(target_os = "linux")]
#[test]
fn a_locked_reader_counts_nothing_after_a_cut_back() {
    let d = tempfile::tempdir().unwrap();
    let (out, _) = failing_sync_with_reader(
        d.path(),
        &std::env::var("PATH").unwrap(),
        r#"sot_inbox_read_lock h || exit 75; sot_inbox_lines h"#,
        true,
    );
    assert_eq!(out.trim(), "1", "a locked reader counted the in-flight line");
}

// (b) The unlocked reader (no flock(1) on its PATH) counts and cursors the
// in-flight line; after the cut-back and one real send its cursor steps
// back one line, says so, and the real line is next — nothing is skipped.
#[cfg(target_os = "linux")]
#[test]
fn an_unlocked_reader_steps_back_one_line_after_a_cut_back() {
    let d = tempfile::tempdir().unwrap();
    let noflock = d.path().join("noflock");
    std::fs::create_dir_all(&noflock).unwrap();
    for dir in std::env::var("PATH").unwrap().split(':') {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name();
                if n != "flock" {
                    let _ = std::os::unix::fs::symlink(e.path(), noflock.join(n));
                }
            }
        }
    }
    std::fs::create_dir_all(d.path().join("read")).unwrap();
    let np = noflock.to_str().unwrap();
    let (out, _) = failing_sync_with_reader(
        d.path(),
        np,
        r#"n="$(sot_inbox_lines h)"; sot_cursor_write h "$n" "$(sed -n "${n}p" "$COMM_HOME/inbox/h.jsonl")"; echo "$n""#,
        false,
    );
    assert_eq!(out.trim(), "2", "the unlocked reader should have counted the in-flight line");
    file_frame(&d.path().join("inbox"), "a", "h", false, "real", "t", Duration::from_secs(5), "local t").unwrap();
    let o = shell(d.path(), np, r#"sot_cursor_offset h"#);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "1");
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("the last line read from @h's inbox was cut back; reading from the line before it"));
    assert_eq!(read_lines(&d.path().join("inbox/h.jsonl"))[1]["msg"], "real");
}

// The daemon's lock descriptor is read-write too: a zero-length read on a
// write-only one is EBADF.
#[test]
fn the_lock_file_is_opened_read_write() {
    use std::io::Read;
    let d = tempfile::tempdir().unwrap();
    let mut lock = open_lock(d.path(), "h").unwrap();
    assert_eq!(lock.read(&mut []).unwrap(), 0);
}

// A waiter that gave up never keeps the lock.
#[test]
fn a_waiter_that_gave_up_never_keeps_the_lock() {
    let d = tempfile::tempdir().unwrap();
    let holder = OpenOptions::new()
        .read(true)
        .create(true)
        .append(true)
        .open(d.path().join("h.lock"))
        .unwrap();
    holder.lock().unwrap();
    let w = Duration::from_millis(200);
    assert!(file_frame(d.path(), "a", "h", false, "late", "t", w, "local t").is_err());
    drop(holder);
    file_frame(d.path(), "a", "h", false, "only", "t", Duration::from_secs(10), "local t").unwrap();
    let lines = read_lines(&d.path().join("h.jsonl"));
    assert_eq!((lines.len(), lines[0]["msg"].as_str()), (1, Some("only")));
}

// A holder frees the lock once the filing is seen waiting: a blocking
// wait shows in /proc/locks before the unlock, a polled wait (and, off
// Linux, a blocking one too) is held 1 s.
// `done >= at` proves the filing followed the unlock, under the polled wait
// and under a blocking one alike; a missed wake or a stopped poll ends in
// the bound's error. It promises no duration beyond that.
fn held_then_freed(own: &str, blocks: bool) {
    let d = tempfile::tempdir().unwrap();
    let lock = d.path().join("h.lock");
    let holder = OpenOptions::new().read(true).create(true).append(true).open(&lock).unwrap();
    holder.lock().unwrap();
    let freed = std::thread::spawn(move || {
        #[cfg(target_os = "linux")]
        if blocks {
            until_a_waiter_blocks_on(&lock);
        } else {
            std::thread::sleep(Duration::from_secs(1));
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = blocks;
            std::thread::sleep(Duration::from_secs(1));
        }
        let at = Instant::now();
        drop(holder);
        at
    });
    let r = file_frame(d.path(), "a", "h", false, "x", "t", Duration::from_secs(10), own);
    let done = Instant::now();
    let at = freed.join().unwrap();
    r.unwrap();
    assert!(done >= at, "{own}: filed before the unlock");
}

#[test]
fn under_nfs4_the_lock_is_polled_and_follows_the_unlock() {
    held_then_freed("nfs4 srv:/export", false);
}

#[test]
fn under_a_local_lock_the_wait_blocks_and_follows_the_unlock() {
    for own in ["local m", "none@m"] {
        held_then_freed(own, true);
    }
}

#[test]
fn both_waits_give_the_same_sentence_at_the_bound() {
    let d = tempfile::tempdir().unwrap();
    let holder = OpenOptions::new().read(true).create(true).append(true).open(d.path().join("h.lock")).unwrap();
    holder.lock().unwrap();
    for own in ["nfs4 srv:/export", "local m"] {
        let e = file_frame(d.path(), "a", "h", false, "x", "t", Duration::from_secs(1), own).unwrap_err();
        assert_eq!(e, "the inbox lock for @h was held for 1s — nothing was appended", "{own}");
    }
}

#[test]
fn a_windows_folder_is_own_disk_only_on_a_fixed_non_unc_drive() {
    assert!(windows_own_disk(true, r"\\?\C:\x"));
    assert!(windows_own_disk(true, r"C:\x"));
    assert!(!windows_own_disk(false, r"\\?\C:\x"));
    assert!(!windows_own_disk(true, r"\\?\UNC\srv\share\x"));
    assert!(!windows_own_disk(true, r"\\srv\share\x"));
}

#[cfg(target_os = "linux")]
#[test]
fn on_linux_the_folders_lock_identity_says_own_disk() {
    let d = Path::new("/x");
    assert!(own_disk(d, "local m"));
    assert!(!own_disk(d, "nfs4 A:/x"));
    assert!(!own_disk(d, "none"));
}

// B1 — the start-time decision, every row.
#[test]
fn only_the_hub_writes_the_record_and_only_over_its_own_machines() {
    let p = Path::new("/c/inbox-lock-manager");
    let at = |role, own, rec: Option<&str>| at_start(role, own, Some("m1"), rec, p);
    assert_eq!(at(Role::Hub, "nfs4 A:/x", None), AtStart::Create);
    assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\nm2\n")), AtStart::Current);
    assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\n")), AtStart::Current);
    assert_eq!(at(Role::Hub, "none@m1", Some("none@m1\nm1\n")), AtStart::Current);
    assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("none@m1\nm1\n")), AtStart::Replace);
    assert_eq!(at(Role::Hub, "none@m1", Some("nfs4 A:/x\nm1\n")), AtStart::Replace);
    for rec in ["none@m2\nm2\n", "nfs4 B:/y\nm2\n", "none@m1\n", "none"] {
        let AtStart::Keep(why) = at(Role::Hub, "nfs4 A:/x", Some(rec)) else { panic!("{rec:?} overwritten") };
        assert!(why.contains("stop every daemon on this comm folder, delete /c/inbox-lock-manager"), "{why}");
    }
    // No machine id is never "written here", not even over a record with no line 2.
    assert!(matches!(at_start(Role::Hub, "none", None, Some("nfs4 A:/x\n"), p), AtStart::Keep(_)));
    assert!(matches!(at(Role::Guest, "nfs4 A:/x", None), AtStart::Keep(_)));
    assert!(matches!(at(Role::Guest, "nfs4 A:/x", Some("nfs4 B:/y\nm1\n")), AtStart::Keep(_)));
}

// B1 — the route for one filing, every row.
#[test]
fn the_route_is_local_only_on_a_proven_shared_lock() {
    let p = Path::new("/c/inbox-lock-manager");
    let r = |role, own, rec: Option<&str>, fwd| route(role, own, Some("m1"), rec, fwd, "hub-a", p);
    let refused = |x: Route, frag: &str| match x {
        Route::Refuse(t) => assert!(t.contains(frag), "{t}"),
        other => panic!("{other:?}, want a refusal naming {frag:?}"),
    };
    assert_eq!(r(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\nm2\n"), true), Route::Local);
    assert_eq!(r(Role::Guest, "nfs4 A:/x", Some("nfs4 A:/x\nm2\n"), false), Route::Local);
    assert_eq!(r(Role::Guest, "nfs4 A:/x", Some("nfs4 B:/y\nm2\n"), false), Route::Forward);
    assert_eq!(r(Role::Guest, "nfs4 A:/x", None, false), Route::Forward);
    refused(r(Role::Guest, "nfs4 A:/x", None, true), "is not its folder's hub (hub-a)");
    refused(r(Role::Guest, "none@m1", Some("none@m2\nm2\n"), true), "is not its folder's hub");
    refused(r(Role::Hub, "nfs4 A:/x", None, false), "no inbox lock record at /c/inbox-lock-manager: restart");
    refused(
        r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\nm1\n"), false),
        "is now nfs4 A:/x but its record says nfs4 B:/y: restart this daemon",
    );
    refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\nm2\n"), false), "written by machine m2, a different lock manager");
    refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\n"), true), "written by an unknown machine");
    refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y"), false), "stop every daemon on this comm folder");
}

// An unknown lock binds the folder to one machine: a `none@m1` record is
// local from m1 alone; from m2 a guest forwards and a hub refuses it as
// another lock manager; bare `none` never matches, not even itself.
#[test]
fn a_none_record_is_filed_under_only_on_the_machine_that_wrote_it() {
    let p = Path::new("/c/inbox-lock-manager");
    let rec = Some("none@m1\nm1\n");
    let from = |role, own, mid| route(role, own, Some(mid), rec, false, "h", p);
    assert_eq!(from(Role::Hub, "none@m1", "m1"), Route::Local);
    assert_eq!(from(Role::Guest, "none@m1", "m1"), Route::Local);
    assert_eq!(from(Role::Guest, "none@m2", "m2"), Route::Forward);
    let Route::Refuse(t) = from(Role::Hub, "none@m2", "m2") else { panic!("filed from m2") };
    assert!(t.contains("names none@m1, written by machine m1, a different lock manager from this hub's none@m2"), "{t}");
    assert!(t.contains("delete /c/inbox-lock-manager, then start the hub"), "{t}");
    for (role, rec) in [(Role::Hub, "none\n"), (Role::Hub, "none"), (Role::Guest, "none\n")] {
        assert_ne!(route(role, "none", None, Some(rec), false, "h", p), Route::Local, "{rec:?}");
    }
    assert!(matches!(at_start(Role::Hub, "none@m2", Some("m2"), rec, p), AtStart::Keep(_)));
    assert_eq!(at_start(Role::Hub, "nfs4 A:/x", Some("m1"), rec, p), AtStart::Replace);
}

// A daemon with no machine id is bare `none`: as the hub it never creates
// or replaces the record and refuses every filing, naming the recovery;
// as a guest it still forwards.
#[test]
fn a_hub_with_no_machine_id_says_so_and_never_writes_the_record() {
    let p = Path::new("/c/inbox-lock-manager");
    let want = refusal::no_machine_id(p);
    assert!(want.contains("no machine id") && want.contains("/etc/machine-id") && want.contains("restart the daemon"), "{want}");
    for rec in [None, Some("none\n"), Some("none"), Some("nfs4 A:/x\nm1\n")] {
        assert_eq!(at_start(Role::Hub, "none", None, rec, p), AtStart::Keep(want.clone()), "{rec:?}");
        assert_eq!(route(Role::Hub, "none", None, rec, false, "h", p), Route::Refuse(want.clone()), "{rec:?}");
    }
    assert_eq!(route(Role::Guest, "none", None, Some("nfs4 A:/x\nm1\n"), false, "h", p), Route::Forward);
    assert_eq!(route(Role::Guest, "none", None, None, false, "h", p), Route::Forward);
}

// B1 — the start step on real files: an absent record is created with the
// machine id on line 2 and no temp file left; a current one is left alone,
// another machine's is left byte-identical, and this machine's stale one
// is replaced.
#[test]
fn the_start_step_keeps_another_machines_record_and_replaces_its_own() {
    let d = tempfile::tempdir().unwrap();
    let rec = d.path().join(LOCK_RECORD);
    let start = |own: &str| record_at_start(d.path(), true, own, Some("m1")).unwrap();
    let only_the_record = || assert_eq!(names(d.path()), [LOCK_RECORD], "a temp file was left");
    assert_eq!(start("none@m1"), (Role::Hub, AtStart::Create));
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), "none@m1\nm1\n");
    only_the_record();
    assert_eq!(start("none@m1").1, AtStart::Current);

    std::fs::write(&rec, "nfs4 B:/y\nm2\n").unwrap();
    assert!(matches!(start("none@m1").1, AtStart::Keep(_)));
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), "nfs4 B:/y\nm2\n");

    std::fs::write(&rec, "nfs4 B:/y\nm1\n").unwrap();
    assert_eq!(start("none@m1").1, AtStart::Replace);
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), "none@m1\nm1\n");
    only_the_record();
}

// B1 — a create over a standing record fails with the link's own
// `AlreadyExists` and leaves the record as it was, with no temp file; the
// start step decides over that record: this daemon's own is current,
// another machine's the lost race's refusal.
#[test]
fn a_create_over_a_standing_record_fails_and_leaves_it_to_decide() {
    let d = tempfile::tempdir().unwrap();
    let rec = d.path().join(LOCK_RECORD);
    let start = || record_at_start(d.path(), true, "none@m1", Some("m1")).unwrap();
    for (writer, mid) in [("none@m1", "m1"), ("none@m2", "m2")] {
        std::fs::write(&rec, record_text(writer, Some(mid))).unwrap();
        let e = create_lock_record(d.path(), "none@m1", Some("m1")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read_to_string(&rec).unwrap(), record_text(writer, Some(mid)));
        assert_eq!(names(d.path()), [LOCK_RECORD], "no temp file");
    }
    std::fs::write(&rec, record_text("none@m1", Some("m1"))).unwrap();
    assert_eq!(start(), (Role::Hub, AtStart::Current));
    std::fs::write(&rec, record_text("none@m2", Some("m2"))).unwrap();
    assert!(matches!(start(), (Role::Hub, AtStart::Keep(_))));
}

// B1 — a link that fails with no record to re-read returns the link's
// own error, never the re-read's and never a create: here the record's
// name is a directory, so the link fails `AlreadyExists` and the re-read
// `IsADirectory`.
#[test]
fn a_failed_link_with_no_record_to_re_read_returns_the_links_error() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join(LOCK_RECORD)).unwrap();
    let e = record_at_start(d.path(), true, "none@m1", Some("m1")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(names(d.path()), [LOCK_RECORD], "no temp file");
}

// B1 — the first read finds nothing, the link fails because another
// holder's record appeared in between, and the re-read finds it: the
// start step decides over it and refuses, it does not return `Current`.
#[test]
fn a_record_that_appears_after_the_first_read_decides_the_start() {
    let d = tempfile::tempdir().unwrap();
    let rec = d.path().join(LOCK_RECORD);
    BETWEEN_READ_AND_LINK.with(|h| {
        *h.borrow_mut() = Some(Box::new(|p| std::fs::write(p, record_text("none@m2", Some("m2"))).unwrap()));
    });
    let got = record_at_start(d.path(), true, "none@m1", Some("m1"));
    BETWEEN_READ_AND_LINK.with(|h| *h.borrow_mut() = None);
    let (_, decision) = got.unwrap();
    let AtStart::Keep(msg) = decision else { panic!("{decision:?}") };
    assert_eq!(msg, refusal::foreign("none@m2", Some("m2"), "none@m1", &rec));
    assert_eq!(std::fs::read_to_string(&rec).unwrap(), record_text("none@m2", Some("m2")));
    assert_eq!(names(d.path()), [LOCK_RECORD], "no temp file");
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect()
}

/// Two hub-less daemons' start steps on one fresh folder, released
/// together: what each did and the one record left.
fn race(ids: [(&'static str, &'static str); 2]) -> (tempfile::TempDir, Vec<AtStart>, String) {
    let d = tempfile::tempdir().unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let runs: Vec<_> = ids
        .into_iter()
        .map(|(own, mid)| {
            let (home, gate) = (d.path().to_path_buf(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                record_at_start(&home, true, own, Some(mid)).unwrap().1
            })
        })
        .collect();
    let did = runs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(names(d.path()), [LOCK_RECORD], "one record and no temp file");
    let text = std::fs::read_to_string(d.path().join(LOCK_RECORD)).unwrap();
    (d, did, text)
}

// Two hub-less daemons started together: exactly one writes the record,
// it is the winner's, and the loser's filing is refused with the recovery
// named. On one shared `nfs4` lock manager both file.
#[test]
fn two_daemons_started_together_leave_one_record() {
    for _ in 0..50 {
        let ids = [("none@m1", "m1"), ("none@m2", "m2")];
        let (d, did, text) = race(ids);
        let w = did.iter().position(|x| *x == AtStart::Create).expect("one daemon created it");
        assert!(matches!(did[1 - w], AtStart::Keep(_)), "{did:?}");
        let ((own_w, mid_w), (own_l, mid_l)) = (ids[w], ids[1 - w]);
        assert_eq!(text, format!("{own_w}\n{mid_w}\n"));
        let path = d.path().join(LOCK_RECORD);
        assert_eq!(route(Role::Hub, own_w, Some(mid_w), Some(&text), false, "h", &path), Route::Local);
        let Route::Refuse(t) = route(Role::Hub, own_l, Some(mid_l), Some(&text), false, "h", &path) else {
            panic!("the loser filed")
        };
        assert!(t.contains(&format!("delete {}, then start the hub", path.display())), "{t}");

        let ids = [("nfs4 A:/x", "m1"), ("nfs4 A:/x", "m2")];
        let (d, did, text) = race(ids);
        let created = did.iter().filter(|x| **x == AtStart::Create).count();
        assert!(created == 1 && did.contains(&AtStart::Current), "{did:?}");
        let path = d.path().join(LOCK_RECORD);
        for (own, mid) in ids {
            assert_eq!(route(Role::Hub, own, Some(mid), Some(&text), false, "h", &path), Route::Local);
        }
    }
}

#[test]
fn a_host_uuid_is_its_canonical_uppercase_text() {
    let b = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
    assert_eq!(uuid_text(&b), "01234567-89AB-CDEF-0123-456789ABCDEF");
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../comm/tests/fixtures/inbox-lock-identity")
}

// Identity parity — the fixture set `test-hub-files.sh` reads too: nfs4,
// nfs (v3), ext4, fuse, a stacked mount and an escaped space.
#[test]
fn identity_from_matches_every_shared_fixture() {
    let dir = fixtures();
    let mid = std::fs::read_to_string(dir.join("machine-id")).unwrap();
    let cases = std::fs::read_to_string(dir.join("cases.tsv")).unwrap();
    let mut n = 0;
    for c in cases.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let [file, path, want]: [&str; 3] = c.split('\t').collect::<Vec<_>>().try_into().unwrap();
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        assert_eq!(identity_from(&text, Path::new(path), mid.trim()), want, "{file}");
        n += 1;
    }
    assert_eq!(n, 7);
}

// The prefix walk, which `findmnt -F` cannot be asked about: the longest
// mount point by whole components; an unknown lock is its machine's, and
// with no machine id it is bare `none`.
#[test]
fn identity_from_takes_the_longest_whole_component_prefix() {
    let text = std::fs::read_to_string(fixtures().join("nfs4-home.mountinfo")).unwrap();
    let at = |p: &str, mid: &str| identity_from(&text, Path::new(p), mid);
    assert_eq!(at("/fixture-home/u/.sot-comm/inbox", "m"), "nfs4 filer.example:/export/home");
    assert_eq!(at("/fixture-homework/inbox", "m"), "local m");
    assert_eq!(at("/fixture-homework/inbox", ""), "none");
    assert_eq!(identity_from("", Path::new("/x"), "m"), "none@m");
    assert_eq!(identity_from("", Path::new("/x"), ""), "none");
    let v3 = std::fs::read_to_string(fixtures().join("nfs3-home.mountinfo")).unwrap();
    assert_eq!(identity_from(&v3, Path::new("/fixture-home"), "m"), "none@m");
    assert_eq!(identity_from(&v3, Path::new("/fixture-home"), ""), "none");
}
