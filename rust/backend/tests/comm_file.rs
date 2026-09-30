#![cfg(target_os = "linux")]
//! The inbox lock (0031 B1) across the two writers that take it: the
//! daemon's filer (`comm_inbox::file_frame`, included by path because this
//! crate is a binary) and the scripts' `sot_inbox_append` (`comm-lib.sh`).
//! Both are `flock(2)` on the same sidecar, so a mixed run is the proof they
//! take the SAME lock, not two that merely look alike.
//!
//! The `#[ignore]`d cases are driven by `comm/core/tests/
//! test-inbox-lock-twohost.sh` against the directory named in
//! `SOT_TEST_INBOX_DIR` — a folder on the shared home, which is where
//! in-process and cross-box exclusion have to be proved rather than assumed.

#[path = "../src/comm_inbox.rs"]
#[allow(dead_code)] // the forward's sentence is the binary's alone
mod comm_inbox;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use comm_inbox::{
    create_lock_record, file_frame, inbox_lock_wait, lock_identity, machine_id, record_at_start, route, AtStart, Role, Route,
    LOCK_RECORD,
};

fn comm_lib() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/core/scripts/comm-lib.sh")
}

/// Every line one JSON object, LF-terminated; returns each line's `msg`.
fn whole_lines(p: &Path) -> Vec<String> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    assert!(s.is_empty() || s.ends_with('\n'), "a partial tail: {s:?}");
    s.lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("one JSON object per line");
            v["msg"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

fn assert_distinct(msgs: &[String], want: usize) {
    let mut seen = msgs.to_vec();
    seen.sort();
    seen.dedup();
    assert_eq!((msgs.len(), seen.len()), (want, want), "a line was lost or doubled");
}

fn inbox_home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("inbox")).unwrap();
    d
}

/// A holder of `inbox/<h>.lock`: takes the lock, then runs `body`. `exec` so
/// the child's pid IS the lock holder — a grandchild that inherited the fd
/// would keep the lock past the kill.
fn holder(inbox: &Path, h: &str, body: &str) -> Child {
    let ready = inbox.join(format!("{h}.ready"));
    let child = Command::new("bash")
        .arg("-c")
        .arg(format!(
            r#"exec 9>> "$1/$2.lock"; flock 9; exec 8>> "$1/$2.jsonl"; touch "$3"; {body}"#
        ))
        .args(["_", inbox.to_str().unwrap(), h, ready.to_str().unwrap()])
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !ready.exists() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the holder never took the lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Sourced after `comm-lib.sh`: the script arm's daemon is one that is not
/// there, so no case can reach a live daemon (with `SOT_SOCKET` removed).
const NO_DAEMON: &str = r#"sot_daemon_endpoint() { printf 'unix:/nonexistent/sot-test.sock'; }"#;

/// `sot_inbox_lock_identity DIR` from the script arm.
fn script_identity(dir: &Path) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(r#"source "$1"; {NO_DAEMON}; sot_inbox_lock_identity "$2""#))
        .args(["_", comm_lib().to_str().unwrap(), dir.to_str().unwrap()])
        .env_remove("SOT_SOCKET")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim_end().to_string()
}

// Identity parity on the real mounts: the daemon's mountinfo walk and the
// script's `findmnt -T` name the same lock manager for the same folders.
#[test]
fn the_filer_and_the_script_name_the_same_lock_manager() {
    let d = inbox_home();
    let dirs = [d.path().join("inbox"), PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from("/")];
    for dir in &dirs {
        assert_eq!(lock_identity(dir), script_identity(dir), "{}", dir.display());
    }
}

// T3 (mixed) — the filer and `sot_inbox_append` on one inbox at once. The
// record comes from the daemon's own writer; a script appends locally only
// when it computes the same identity, so this is parity on a real mount too.
#[test]
fn the_filer_and_the_script_take_the_same_lock() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let id = lock_identity(&inbox);
    create_lock_record(d.path(), &id, machine_id().as_deref()).unwrap();
    assert_ne!(id, "none", "this test needs a folder whose lock manager is provable");
    let script = {
        let home = d.path().to_path_buf();
        std::thread::spawn(move || {
            let st = Command::new("bash")
                .arg("-c")
                .arg(format!(r#"source "$1"; {NO_DAEMON}; for i in $(seq 0 199); do
                        printf '{{"from":"sh","to":"t3","repo":"r","msg":"sh-%s","ts":"t"}}\n' "$i" \
                          | sot_inbox_append t3 >/dev/null || exit 1; done"#))
                .args(["_", comm_lib().to_str().unwrap()])
                .env_remove("SOT_SOCKET")
                .env("SOT_COMM_HOME", &home)
                .status()
                .unwrap();
            assert!(st.success(), "the script arm refused a line");
        })
    };
    for i in 0..200 {
        file_frame(&inbox, "rust", "t3", false, &format!("rust-{i}"), "t", inbox_lock_wait(), "local t").unwrap();
    }
    script.join().unwrap();
    assert_distinct(&whole_lines(&inbox.join("t3.jsonl")), 400);
}

// T12 (Rust arm) — a killed holder costs nothing: the OS released the lock.
#[test]
fn a_killed_holder_frees_the_lock_at_once() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let mut h = holder(&inbox, "k", "exec sleep 60");
    h.kill().unwrap();
    h.wait().unwrap();
    let t0 = Instant::now();
    file_frame(&inbox, "rust", "k", false, "after", "t", Duration::from_secs(10), "local t").unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2), "waited {:?} for a dead holder", t0.elapsed());
    assert_eq!(whole_lines(&inbox.join("k.jsonl")), ["after"]);
}

// T12 (Rust arm) — a frozen holder: the filer waits, fails, appends nothing;
// the holder resumes into a whole line; the next append files.
#[test]
fn a_frozen_holder_makes_the_filer_wait_then_fail() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let mut h = holder(
        &inbox,
        "f",
        r#"printf '%s' '{"from":"holder",' >&8; kill -STOP $$; printf '%s\n' '"msg":"resumed"}' >&8"#,
    );
    let t0 = Instant::now();
    let e = file_frame(&inbox, "rust", "f", false, "frozen", "t", Duration::from_secs(1), "local t").unwrap_err();
    assert!(t0.elapsed() >= Duration::from_secs(1));
    assert_eq!(e, "the inbox lock for @f was held for 1s — nothing was appended");
    let st = Command::new("kill").args(["-CONT", &h.id().to_string()]).status().unwrap();
    assert!(st.success());
    h.wait().unwrap();
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed"]);
    file_frame(&inbox, "rust", "f", false, "after", "t", Duration::from_secs(1), "local t").unwrap();
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed", "after"]);
}

// S2 (Rust arm) — a write the file-size limit cuts off mid-line: the filer
// answers the append failed and the file is byte-identical. The filer runs
// in a child (this binary, the ignored case below) under `ulimit -f 1`
// (1024 bytes) with SIGXFSZ ignored, so the write returns EFBIG after a
// partial write instead of killing it. The fsync itself is read, not tested.
#[test]
fn a_write_cut_short_by_the_file_size_limit_leaves_the_file_byte_identical() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let before = format!("{{\"msg\":\"{}\"}}\n", "x".repeat(1000 - 11));
    assert_eq!(before.len(), 1000);
    std::fs::write(inbox.join("z.jsonl"), &before).unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#"ulimit -f 1; trap '' XFSZ; exec "$@""#)
        .args(["_", std::env::current_exe().unwrap().to_str().unwrap()])
        .args(["--exact", "fsize_child_files_one", "--ignored", "--nocapture", "--test-threads=1"])
        .env("SOT_TEST_INBOX_DIR", &inbox)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("FAILED the append failed: "), "{stdout}");
    assert_eq!(std::fs::read_to_string(inbox.join("z.jsonl")).unwrap(), before);
}

#[test]
#[ignore = "run by a_write_cut_short_by_the_file_size_limit_leaves_the_file_byte_identical"]
fn fsize_child_files_one() {
    match file_frame(&env_dir(), "rust", "z", false, &"y".repeat(200), "t", Duration::from_secs(1), "local t") {
        Ok(()) => println!("filed"),
        Err(e) => println!("FAILED {e}"),
    }
}

fn env_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("SOT_TEST_INBOX_DIR").expect("SOT_TEST_INBOX_DIR"))
}

// T3 on the shared home — two threads of ONE process: flock on a network
// mount is emulated, so in-process exclusion there is a fact to prove.
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn two_threads_on_the_env_dir() {
    let inbox = env_dir();
    let threads: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|w| {
            let inbox = inbox.clone();
            std::thread::spawn(move || {
                for i in 0..200 {
                    file_frame(&inbox, w, "t3", false, &format!("{w}-{i}"), "t", inbox_lock_wait(), &lock_identity(&inbox)).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_distinct(&whole_lines(&inbox.join("t3.jsonl")), 400);
}

// T11 (a) — the Rust side of a two-box run: 200 lines, one verdict each,
// started together with the other side: `rust.ready` says this side is up,
// and the driver's `go`, created on this box, releases it.
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn t11_rust_appends_200() {
    let inbox = env_dir();
    std::fs::write(inbox.join("rust.ready"), "").unwrap();
    let t0 = Instant::now();
    while !inbox.join("go").exists() {
        assert!(t0.elapsed() < Duration::from_secs(120), "no go from the driver");
        std::thread::sleep(Duration::from_millis(5));
    }
    for i in 0..200 {
        let msg = format!("rust-{i}");
        match file_frame(&inbox, "rust", "t11", false, &msg, "t", inbox_lock_wait(), &lock_identity(&inbox)) {
            Ok(()) => println!("filed {msg}"),
            Err(e) => println!("FAILED {msg}: {e}"),
        }
        // Paced near the shell arm's fork-per-line rate, so the two sides
        // overlap instead of this one finishing before the other starts.
        // A remote waiter's NFS lock retries back off from ~100 ms, so a burst shorter than that can finish before the other host gets one turn and the case would prove no concurrency.
        // The unpaced liveness case sets SOT_T11_PACE_MS=0.
        let pace = std::env::var("SOT_T11_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(25);
        std::thread::sleep(Duration::from_millis(pace));
    }
}

// T11 — the record the hub writes at startup, into the driver's comm folder
// (the parent of `SOT_TEST_INBOX_DIR`): this box is that folder's hub.
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn t11_write_lock_record() {
    std::fs::create_dir_all(env_dir()).unwrap();
    let id = lock_identity(&env_dir());
    let (role, did) = record_at_start(env_dir().parent().unwrap(), true, &id, machine_id().as_deref()).unwrap();
    assert_eq!((role, did), (Role::Hub, AtStart::Create));
    println!("record {id}");
}

// The one-host case (test-inbox-lock-onehost.sh), the daemon's shape: four
// filer threads in ONE process on the machine the record binds, 50 long lines
// each, once its route says this machine files locally.
#[test]
#[ignore = "driven by test-inbox-lock-onehost.sh"]
fn onehost_four_filer_threads() {
    let inbox = env_dir();
    let home = inbox.parent().unwrap().to_path_buf();
    let record = std::fs::read_to_string(home.join(LOCK_RECORD)).ok();
    let own = lock_identity(&inbox);
    let r = route(Role::Hub, &own, machine_id().as_deref(), record.as_deref(), false, "onehost", &home.join(LOCK_RECORD));
    println!("route {own}: {r:?}");
    assert_eq!(r, Route::Local);
    let t0 = Instant::now();
    while !inbox.join("go").exists() {
        assert!(t0.elapsed() < Duration::from_secs(120), "no go from the driver");
        std::thread::sleep(Duration::from_millis(5));
    }
    let pad = "x".repeat(2048);
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let (inbox, pad) = (inbox.clone(), pad.clone());
            std::thread::spawn(move || {
                for i in 0..50 {
                    let key = format!("rust{t}-{i}");
                    match file_frame(&inbox, &format!("rust{t}"), "t1h", false, &format!("{key} {pad}"), "t", inbox_lock_wait(), &lock_identity(&inbox)) {
                        Ok(()) => println!("filed {key}"),
                        Err(e) => println!("FAILED {key}: {e}"),
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

// T11 (b), (c) — one send, its verdict and how long it took.
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn t11_rust_sends_one() {
    let inbox = env_dir();
    let msg = format!("rust-one-{}", std::process::id());
    let t0 = Instant::now();
    let r = file_frame(&inbox, "rust", "t11", false, &msg, "t", inbox_lock_wait(), &lock_identity(&inbox));
    let ms = t0.elapsed().as_millis();
    match r {
        Ok(()) => println!("filed {msg} after {ms}ms"),
        Err(e) => println!("FAILED {msg}: {e} after {ms}ms"),
    }
}
