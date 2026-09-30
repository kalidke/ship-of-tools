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
//! on a disk other hosts mount does not exclude their NFS locks. So the
//! folder's hub names its lock manager in `<comm home>/inbox-lock-manager` at
//! startup, and every writer — a script, or a daemon at each filing — appends
//! locally only when it computes the same name for the inbox by the same
//! rule; a script sends anything else to a daemon, a guest daemon forwards it
//! to the hub, and the hub refuses it with the recovery named.
//!
//! std and serde only, so `tests/comm_file.rs` can include this file by path
//! and drive the real filer from outside a binary-only crate.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
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

/// `line` goes in whole or not at all ("filed" means kept): a torn tail (a
/// writer that died mid-line) is ended first so it stays its own line, the
/// bytes are flushed to disk on this descriptor before `Ok`, and any error
/// cuts the file back to its length before.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f: File = OpenOptions::new().create(true).read(true).append(true).open(path)?;
    let len = f.metadata()?.len();
    let mut last = [b'\n'];
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
    }
    let mut bytes = Vec::with_capacity(line.len() + 1);
    if last[0] != b'\n' {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(line.as_bytes());
    let written = f.write_all(&bytes).and_then(|()| f.sync_data());
    if written.is_err() {
        let _ = f.set_len(len);
    }
    written
}

/// The lock record's name, beside `registry.json` — never in `inbox/`, and
/// nothing globs for it.
pub const LOCK_RECORD: &str = "inbox-lock-manager";

/// Local block filesystems: a lock on one is this kernel's own.
const LOCAL_FS: [&str; 7] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs"];

/// Write `<comm_home>/inbox-lock-manager` (a temp file, then a rename): line
/// 1 the lock manager `id` appends to `<comm_home>/inbox` go through, line 2
/// the host that wrote it. Only the folder's hub calls this.
pub fn write_lock_record(comm_home: &Path, id: &str, self_host: &str) -> std::io::Result<()> {
    let tmp = comm_home.join(format!(".{LOCK_RECORD}.{}", std::process::id()));
    std::fs::write(&tmp, format!("{id}\n{self_host}\n"))?;
    std::fs::rename(&tmp, comm_home.join(LOCK_RECORD))
}

/// Who a daemon is to its comm folder, evaluated at each use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Writes the record and never forwards a filing.
    Hub,
    /// Files only on a proven shared lock; everything else goes to the hub.
    Guest,
}

/// The folder's hub when `topology_hub` (no topology file loads, or it names
/// this host as hub) or the folder is on this box's own disk: not Linux, or
/// `own` is `local <machine-id>` — a Windows or rig daemon files for its own
/// comm folder, which no hub shares.
pub fn role(topology_hub: bool, own: &str) -> Role {
    if topology_hub || !cfg!(target_os = "linux") || own.starts_with("local ") {
        Role::Hub
    } else {
        Role::Guest
    }
}

/// The record's two lines: the lock manager, and the host that wrote it —
/// `None` when there is no second line, an unknown writer that is never "mine".
pub fn parse_record(text: &str) -> (&str, Option<&str>) {
    let mut lines = text.lines();
    let id = lines.next().unwrap_or("");
    (id, lines.next().filter(|w| !w.is_empty()))
}

/// What the hub does with the record when it starts.
#[derive(Debug, PartialEq, Eq)]
pub enum AtStart {
    Write,
    /// Left untouched, and why.
    Keep(String),
}

/// The start-time decision: only the hub writes, and it replaces a record
/// naming another lock manager only when this host wrote that record (the
/// hub after a remount). A guest never writes or deletes it.
pub fn at_start(role: Role, own: &str, record: Option<&str>, self_host: &str, path: &Path) -> AtStart {
    if role == Role::Guest {
        return AtStart::Keep(format!(
            "{path}: this daemon is a guest on its folder's hub, which alone writes the inbox lock record",
            path = path.display()
        ));
    }
    let Some((id, writer)) = record.map(parse_record) else {
        return AtStart::Write;
    };
    if id == own || writer == Some(self_host) {
        AtStart::Write
    } else {
        AtStart::Keep(refusal::foreign(id, writer, own, path))
    }
}

/// Where one `comm.file` goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// This daemon appends under the inbox lock.
    Local,
    /// A guest sends the request to the hub.
    Forward,
    /// `file_failed` with the recovery named; nothing is appended.
    Refuse(String),
}

/// The route for one filing, from this daemon's own lock manager `own`
/// (recomputed per filing) and the record. A `none` record is safe only for
/// its hub: no script appends locally under `none` and guests forward, so the
/// hub is the folder's only writer.
pub fn route(role: Role, own: &str, record: Option<&str>, forwarded: bool, self_host: &str, path: &Path) -> Route {
    let rec = record.map(parse_record);
    if rec.is_some_and(|(id, _)| id == own && (own != "none" || role == Role::Hub)) {
        return Route::Local;
    }
    match (role, rec) {
        (Role::Guest, _) if forwarded => Route::Refuse(refusal::forwarded_to_guest(self_host)),
        (Role::Guest, _) => Route::Forward,
        (Role::Hub, None) => Route::Refuse(refusal::no_record(path)),
        (Role::Hub, Some((id, writer))) if writer == Some(self_host) => Route::Refuse(refusal::remounted(own, id)),
        (Role::Hub, Some((id, writer))) => Route::Refuse(refusal::foreign(id, writer, own, path)),
    }
}

/// Every sentence a refused filing or a kept record prints, in one place.
pub mod refusal {
    use std::path::Path;

    pub fn no_record(path: &Path) -> String {
        format!("no inbox lock record at {}: restart this daemon to write it", path.display())
    }

    pub fn remounted(own: &str, rec: &str) -> String {
        format!(
            "this hub's inbox lock is now {own} but its record says {rec}: restart this daemon to re-record it after the remount"
        )
    }

    pub fn foreign(rec: &str, writer: Option<&str>, own: &str, path: &Path) -> String {
        format!(
            "the inbox lock record says {rec} (written by {}), not this hub's {own}: stop every daemon on this comm folder, delete {}, then start the hub",
            writer.unwrap_or("an unknown host"),
            path.display()
        )
    }

    pub fn forwarded_to_guest(self_host: &str) -> String {
        format!("a forwarded frame reached a daemon that is not its folder's hub ({self_host})")
    }

    pub fn hub_did_not_answer(endpoint: &str, err: &str) -> String {
        format!("the hub did not answer at {endpoint}: {err}")
    }
}

/// The hub's start step: create `inbox/` (its lock manager is computed for
/// `inbox/`, as the scripts compute it), then decide and write. Returns the
/// role, this daemon's lock manager and what it did.
pub fn record_at_start(comm_home: &Path, topology_hub: bool, self_host: &str) -> std::io::Result<(Role, String, AtStart)> {
    std::fs::create_dir_all(comm_home.join("inbox"))?;
    let own = lock_identity(&comm_home.join("inbox"));
    let role = role(topology_hub, &own);
    let path = comm_home.join(LOCK_RECORD);
    let record = std::fs::read_to_string(&path).ok();
    let decision = at_start(role, &own, record.as_deref(), self_host, &path);
    if decision == AtStart::Write {
        write_lock_record(comm_home, &own, self_host)?;
    }
    Ok((role, own, decision))
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

    // S2 — a torn tail stays its own line and the new line stays whole.
    #[test]
    fn a_torn_tail_is_ended_before_the_new_line() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h.jsonl"), "{\"from\":\"died\",").unwrap();
        file_frame(d.path(), "a", "h", false, "whole", "t", Duration::from_secs(1)).unwrap();
        let raw = std::fs::read_to_string(d.path().join("h.jsonl")).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines[0], "{\"from\":\"died\",");
        let v: serde_json::Value = serde_json::from_str(lines[1]).expect("the new line parses");
        assert_eq!((lines.len(), v["msg"].as_str()), (2, Some("whole")));
    }

    // B1 — the role: the topology's hub, or a folder on this box's own disk.
    #[test]
    fn the_role_is_hub_by_topology_or_by_own_disk() {
        assert_eq!(role(true, "nfs4 A:/x"), Role::Hub);
        assert_eq!(role(true, "none"), Role::Hub);
        assert_eq!(role(false, "local m"), Role::Hub);
        let guest = if cfg!(target_os = "linux") { Role::Guest } else { Role::Hub };
        assert_eq!(role(false, "nfs4 A:/x"), guest);
        assert_eq!(role(false, "none"), guest);
    }

    // B1 — the start-time decision, every row.
    #[test]
    fn only_the_hub_writes_the_record_and_only_over_its_own() {
        let p = Path::new("/c/inbox-lock-manager");
        let at = |role, own, rec: Option<&str>| at_start(role, own, rec, "hub-a", p);
        assert_eq!(at(Role::Hub, "nfs4 A:/x", None), AtStart::Write);
        assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\nhub-b\n")), AtStart::Write);
        assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\n")), AtStart::Write);
        assert_eq!(at(Role::Hub, "nfs4 A:/x", Some("none\nhub-a\n")), AtStart::Write);
        for rec in ["none\nhub-b\n", "none\n", "none"] {
            let AtStart::Keep(why) = at(Role::Hub, "nfs4 A:/x", Some(rec)) else { panic!("{rec:?} overwritten") };
            assert!(why.contains("stop every daemon on this comm folder, delete /c/inbox-lock-manager"), "{why}");
        }
        assert!(matches!(at(Role::Guest, "nfs4 A:/x", None), AtStart::Keep(_)));
        assert!(matches!(at(Role::Guest, "nfs4 A:/x", Some("nfs4 B:/y\nguest-b\n")), AtStart::Keep(_)));
    }

    // B1 — the route for one filing, every row.
    #[test]
    fn the_route_is_local_only_on_a_proven_shared_lock() {
        let p = Path::new("/c/inbox-lock-manager");
        let r = |role, own, rec: Option<&str>, fwd| route(role, own, rec, fwd, "hub-a", p);
        let refused = |x: Route, frag: &str| match x {
            Route::Refuse(t) => assert!(t.contains(frag), "{t}"),
            other => panic!("{other:?}, want a refusal naming {frag:?}"),
        };
        assert_eq!(r(Role::Hub, "none", Some("none\nhub-a\n"), false), Route::Local);
        assert_eq!(r(Role::Hub, "nfs4 A:/x", Some("nfs4 A:/x\nhub-b\n"), true), Route::Local);
        assert_eq!(r(Role::Guest, "nfs4 A:/x", Some("nfs4 A:/x\nhub-a\n"), false), Route::Local);
        assert_eq!(r(Role::Guest, "none", Some("none\nhub-a\n"), false), Route::Forward);
        assert_eq!(r(Role::Guest, "nfs4 A:/x", Some("nfs4 B:/y\nhub-a\n"), false), Route::Forward);
        assert_eq!(r(Role::Guest, "nfs4 A:/x", None, false), Route::Forward);
        refused(r(Role::Guest, "nfs4 A:/x", None, true), "is not its folder's hub (hub-a)");
        refused(r(Role::Guest, "none", Some("none\n"), true), "is not its folder's hub");
        refused(r(Role::Hub, "nfs4 A:/x", None, false), "no inbox lock record at /c/inbox-lock-manager: restart");
        refused(
            r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\nhub-a\n"), false),
            "is now nfs4 A:/x but its record says nfs4 B:/y: restart this daemon",
        );
        refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\nhub-b\n"), false), "(written by hub-b)");
        refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y\n"), true), "(written by an unknown host)");
        refused(r(Role::Hub, "nfs4 A:/x", Some("nfs4 B:/y"), false), "stop every daemon on this comm folder");
    }

    // B1 — the stale record at start, on real files: another writer's is
    // left byte-identical, this host's own stale one is rewritten, and an
    // absent one is written.
    #[test]
    fn the_start_step_keeps_another_writers_record_and_rewrites_its_own() {
        let d = tempfile::tempdir().unwrap();
        let rec = d.path().join(LOCK_RECORD);
        let (role, own, did) = record_at_start(d.path(), true, "hub-a").unwrap();
        assert_eq!((role, &did), (Role::Hub, &AtStart::Write));
        assert!(d.path().join("inbox").is_dir(), "inbox/ is created before its lock manager is named");
        assert_eq!(std::fs::read_to_string(&rec).unwrap(), format!("{own}\nhub-a\n"));

        std::fs::write(&rec, "nfs4 B:/y\nhub-b\n").unwrap();
        let (_, _, did) = record_at_start(d.path(), true, "hub-a").unwrap();
        assert!(matches!(did, AtStart::Keep(_)), "{did:?}");
        assert_eq!(std::fs::read_to_string(&rec).unwrap(), "nfs4 B:/y\nhub-b\n");

        std::fs::write(&rec, "nfs4 B:/y\nhub-a\n").unwrap();
        assert_eq!(record_at_start(d.path(), true, "hub-a").unwrap().2, AtStart::Write);
        assert_eq!(std::fs::read_to_string(&rec).unwrap(), format!("{own}\nhub-a\n"));
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
}
