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
mod comm_inbox;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use comm_inbox::{file_frame, inbox_lock_wait, lock_identity, write_lock_record};

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

/// `sot_inbox_lock_identity DIR` from the script arm.
fn script_identity(dir: &Path) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#"source "$1"; sot_inbox_lock_identity "$2""#)
        .args(["_", comm_lib().to_str().unwrap(), dir.to_str().unwrap()])
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
    let id = write_lock_record(d.path()).unwrap();
    assert_ne!(id, "none", "this test needs a folder whose lock manager is provable");
    let script = {
        let home = d.path().to_path_buf();
        std::thread::spawn(move || {
            let st = Command::new("bash")
                .arg("-c")
                .arg(r#"source "$1"; for i in $(seq 0 199); do
                        printf '{"from":"sh","to":"t3","repo":"r","msg":"sh-%s","ts":"t"}\n' "$i" \
                          | sot_inbox_append t3 >/dev/null || exit 1; done"#)
                .args(["_", comm_lib().to_str().unwrap()])
                .env("SOT_COMM_HOME", &home)
                .status()
                .unwrap();
            assert!(st.success(), "the script arm refused a line");
        })
    };
    for i in 0..200 {
        file_frame(&inbox, "rust", "t3", false, &format!("rust-{i}"), "t", inbox_lock_wait()).unwrap();
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
    file_frame(&inbox, "rust", "k", false, "after", "t", Duration::from_secs(10)).unwrap();
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
    let e = file_frame(&inbox, "rust", "f", false, "frozen", "t", Duration::from_secs(1)).unwrap_err();
    assert!(t0.elapsed() >= Duration::from_secs(1));
    assert_eq!(e, "the inbox lock for @f was held for 1s — nothing was appended");
    let st = Command::new("kill").args(["-CONT", &h.id().to_string()]).status().unwrap();
    assert!(st.success());
    h.wait().unwrap();
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed"]);
    file_frame(&inbox, "rust", "f", false, "after", "t", Duration::from_secs(1)).unwrap();
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed", "after"]);
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
                    file_frame(&inbox, w, "t3", false, &format!("{w}-{i}"), "t", inbox_lock_wait()).unwrap();
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
        match file_frame(&inbox, "rust", "t11", false, &msg, "t", inbox_lock_wait()) {
            Ok(()) => println!("filed {msg}"),
            Err(e) => println!("FAILED {msg}: {e}"),
        }
        // Paced near the shell arm's fork-per-line rate, so the two sides
        // overlap instead of this one finishing before the other starts.
        // A remote waiter's NFS lock retries back off from ~100 ms, so a burst shorter than that can finish before the other host gets one turn and the case would prove no concurrency.
        std::thread::sleep(Duration::from_millis(25));
    }
}

// T11 — the record the daemon writes at startup, into the driver's comm
// folder (the parent of `SOT_TEST_INBOX_DIR`).
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn t11_write_lock_record() {
    let id = write_lock_record(env_dir().parent().unwrap()).unwrap();
    println!("record {id}");
}

// T11 (b), (c) — one send, its verdict and how long it took.
#[test]
#[ignore = "driven by test-inbox-lock-twohost.sh"]
fn t11_rust_sends_one() {
    let inbox = env_dir();
    let msg = format!("rust-one-{}", std::process::id());
    let t0 = Instant::now();
    let r = file_frame(&inbox, "rust", "t11", false, &msg, "t", inbox_lock_wait());
    let ms = t0.elapsed().as_millis();
    match r {
        Ok(()) => println!("filed {msg} after {ms}ms"),
        Err(e) => println!("FAILED {msg}: {e} after {ms}ms"),
    }
}
