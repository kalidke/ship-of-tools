//! The inbox append (0031 B1): the daemon's arm of the ONE lock both writers
//! take. `comm-lib.sh`'s `sot_inbox_append` is the other arm, and the two
//! meet at the kernel: `flock(1)` and `File::try_lock` are both `flock(2)` on
//! unix, on the same sidecar `inbox/<handle>.lock`. The OS releases the lock
//! when its holder's handle closes — a kill, a panic, a lost ssh child — so
//! there is no reclaim; a frozen holder only makes the next writer wait, and a
//! writer that cannot take the lock within the wait appends NOTHING.
//!
//! The inbox is opened inside the lock and closed before it is released:
//! correctness is the lock plus close-to-open consistency on a network home,
//! never `O_APPEND`'s offset across two boxes.
//!
//! The lock only excludes writers that go through ONE lock manager: an NFSv3
//! lock and an NFSv4 lock on one export exclude nothing, and a local `flock`
//! on a disk other hosts mount does not exclude their NFS locks. So at startup
//! the daemon names its own lock manager in `<comm home>/inbox-lock-manager`,
//! and a script appends locally only when it computes the same name for the
//! inbox by the same rule; anything else sends to a daemon.
//!
//! std and serde only, so `tests/comm_file.rs` can include this file by path
//! and drive the real filer from outside a binary-only crate.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The one knob, read under the same name as the script arm's.
pub const INBOX_LOCK_WAIT_ENV: &str = "SOT_INBOX_LOCK_WAIT_SECS";
/// The one number: `comm-lib.sh` defaults the same variable to 10.
pub const INBOX_LOCK_WAIT_DEFAULT_SECS: u64 = 10;

/// Poll step while another writer holds the lock.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// The wait a writer spends on a held lock before it gives up.
pub fn inbox_lock_wait() -> Duration {
    let secs = std::env::var(INBOX_LOCK_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(INBOX_LOCK_WAIT_DEFAULT_SECS);
    Duration::from_secs(secs)
}

/// One inbox line, fields in the order the scripts write them.
#[derive(serde::Serialize)]
struct Line<'a> {
    from: &'a str,
    to: &'a str,
    repo: &'a str,
    msg: &'a str,
    ts: &'a str,
}

/// Append one frame to `inbox_dir/<to>.jsonl` under the inbox lock. The line
/// is `{from, to, repo:"daemon", msg, ts}`, LF-terminated — what the relay
/// bridge has always written, so every reader renders it unchanged; a
/// `broadcast` copy is stamped `to:""`, as `comm-send.sh` stamps one. `Err`
/// is the sentence the sender prints after `FAILED -> @<to>: `, and it means
/// nothing was appended.
pub fn file_frame(
    inbox_dir: &Path,
    from: &str,
    to: &str,
    broadcast: bool,
    text: &str,
    ts: &str,
    wait: Duration,
) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("the append failed: {e}");
    let mut line = serde_json::to_string(&Line {
        from,
        to: if broadcast { "" } else { to },
        repo: "daemon",
        msg: text,
        ts,
    })
    .map_err(|e| format!("the append failed: {e}"))?;
    line.push('\n');

    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(inbox_dir.join(format!("{to}.lock")))
        .map_err(failed)?;
    let start = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if start.elapsed() < wait => {
                std::thread::sleep(LOCK_POLL)
            }
            Err(TryLockError::WouldBlock) => {
                return Err(format!(
                    "the inbox lock for @{to} was held for {}s — nothing was appended",
                    wait.as_secs()
                ))
            }
            Err(TryLockError::Error(e)) => return Err(failed(e)),
        }
    }
    let written = append_line(&inbox_dir.join(format!("{to}.jsonl")), &line);
    // The inbox is closed inside `append_line`; only now does the lock go.
    drop(lock);
    written.map_err(failed)
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f: File = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.flush()
}

/// The lock record's name, beside `registry.json` — never in `inbox/`, and
/// nothing globs for it.
pub const LOCK_RECORD: &str = "inbox-lock-manager";

/// Local block filesystems: a lock on one is this kernel's own.
const LOCAL_FS: [&str; 7] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs"];

/// Write `<comm_home>/inbox-lock-manager` (a temp file, then a rename): the
/// lock manager this daemon's appends to `<comm_home>/inbox` go through.
/// Returns what was written.
pub fn write_lock_record(comm_home: &Path) -> std::io::Result<String> {
    let id = lock_identity(&comm_home.join("inbox"));
    let tmp = comm_home.join(format!(".{LOCK_RECORD}.{}", std::process::id()));
    std::fs::write(&tmp, format!("{id}\n"))?;
    std::fs::rename(&tmp, comm_home.join(LOCK_RECORD))?;
    Ok(id)
}

/// The lock manager an append to `dir` goes through: `nfs4 <source>` (an NFS
/// v4 mount with `local_lock=none`), `local <machine-id>`, or `none` — every non-Linux platform, and anything
/// whose lock is not provably one manager's. `comm-lib.sh`'s
/// `sot_inbox_lock_identity` computes the same string with `findmnt -T`.
pub fn lock_identity(dir: &Path) -> String {
    #[cfg(target_os = "linux")]
    {
        let (Ok(path), Ok(mountinfo)) = (
            std::fs::canonicalize(dir),
            std::fs::read_to_string("/proc/self/mountinfo"),
        ) else {
            return "none".into();
        };
        let machine_id = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
        identity_from(&mountinfo, &path, machine_id.trim())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        "none".into()
    }
}

/// `lock_identity` over mountinfo text, for a canonical `path`. The mount is
/// the one `findmnt -T` finds: the longest mount-point prefix, the LAST entry
/// when one is stacked over another, `\040`-style escapes decoded. An `nfs4`
/// mount counts only with `vers=4.x` and `local_lock=none` in its super options.
pub fn identity_from(mountinfo: &str, path: &Path, machine_id: &str) -> String {
    let mut best: Option<(usize, String, String, String)> = None;
    for line in mountinfo.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let mut post = post.split(' ');
        let (Some(point), Some(fstype), Some(source), Some(opts)) =
            (pre.split(' ').nth(4), post.next(), post.next(), post.next())
        else {
            continue;
        };
        let point = PathBuf::from(unescape(point));
        let depth = point.components().count();
        if path.starts_with(&point) && best.as_ref().map_or(true, |b| depth >= b.0) {
            best = Some((depth, unescape(fstype), unescape(source), opts.to_string()));
        }
    }
    match best {
        Some((_, fstype, source, opts))
            if fstype == "nfs4"
                && !source.is_empty()
                && opts.split(',').any(|o| o.starts_with("vers=4."))
                && opts.split(',').any(|o| o == "local_lock=none") =>
        {
            format!("nfs4 {source}")
        }
        Some((_, fstype, _, _)) if LOCAL_FS.contains(&fstype.as_str()) && !machine_id.is_empty() => {
            format!("local {machine_id}")
        }
        _ => "none".into(),
    }
}

/// Decode mountinfo's `\ooo` octal escapes (a space is `\040`).
fn unescape(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let oct = b.get(i + 1..i + 4).filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match (b[i], oct) {
            (b'\\', Some(d)) => {
                out.push(d.iter().fold(0u8, |n, c| n.wrapping_mul(8) + (c - b'0')));
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
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
        file_frame(d.path(), "a", "h", false, "one", "2026-01-02T03:04:05Z", w).unwrap();
        file_frame(d.path(), "a", "h", false, "two\nlines", "2026-01-02T03:04:06Z", w).unwrap();
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
                        file_frame(&dir, w, "t3", false, &text, "t", inbox_lock_wait()).unwrap();
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

    // An append that fails under the lock is an error, and says so.
    #[test]
    fn a_failed_write_is_the_append_failed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("h.jsonl")).unwrap();
        let e = file_frame(d.path(), "a", "h", false, "m", "t", Duration::from_secs(1)).unwrap_err();
        assert!(e.starts_with("the append failed: "), "{e}");
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../comm/core/tests/fixtures/inbox-lock-identity")
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
    // mount point by whole components, and no machine id is no identity.
    #[test]
    fn identity_from_takes_the_longest_whole_component_prefix() {
        let text = std::fs::read_to_string(fixtures().join("nfs4-home.mountinfo")).unwrap();
        let at = |p: &str, mid: &str| identity_from(&text, Path::new(p), mid);
        assert_eq!(at("/fixture-home/u/.sot-comm/inbox", "m"), "nfs4 filer.example:/export/home");
        assert_eq!(at("/fixture-homework/inbox", "m"), "local m");
        assert_eq!(at("/fixture-homework/inbox", ""), "none");
        assert_eq!(identity_from("", Path::new("/x"), "m"), "none");
    }

    // T13 — the default is the script's number.
    #[test]
    fn the_wait_defaults_to_ten() {
        assert_eq!(INBOX_LOCK_WAIT_DEFAULT_SECS, 10);
    }
}
