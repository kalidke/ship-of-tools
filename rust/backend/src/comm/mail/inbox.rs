//! The inbox append (0031 B1): the daemon's arm of the ONE lock both writers
//! take. `comm-lib.sh`'s `sot_inbox_append` is the other arm, and the two
//! meet at the kernel: `flock(1)` and `File::lock` are both `flock(2)` on
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
//! to the hub, and the hub refuses it with the recovery named. An unknown lock
//! is named `none@<machine-id>`, so it binds the folder to the one machine
//! that wrote the record: that machine's processes share its one kernel lock,
//! and every other machine computes a different name.
//!
//! std and serde only, so `tests/comm_file.rs` can include this file by path
//! and drive the real filer from outside a binary-only crate.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The one knob, read under the same name as the script arm's.
pub const INBOX_LOCK_WAIT_ENV: &str = "SOT_INBOX_LOCK_WAIT_SECS";
/// The one number: `comm-lib.sh` defaults the same variable to 10.
pub const INBOX_LOCK_WAIT_DEFAULT_SECS: u64 = 10;

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
/// nothing was appended. The wait is bounded, but a helper thread stays
/// blocked behind a frozen holder until that holder releases or dies; if it
/// gets the lock after the caller gave up, it lets it go at once. `own` is the
/// lock identity this filing runs under; it picks how the lock is waited for
/// (see `take_lock`).
#[allow(clippy::too_many_arguments)]
pub fn file_frame(
    inbox_dir: &Path,
    from: &str,
    to: &str,
    broadcast: bool,
    text: &str,
    ts: &str,
    wait: Duration,
    own: &str,
) -> Result<(), String> {
    file_frame_with(inbox_dir, from, to, broadcast, text, ts, wait, own, File::sync_data)
}

#[allow(clippy::too_many_arguments)]
fn file_frame_with(
    inbox_dir: &Path,
    from: &str,
    to: &str,
    broadcast: bool,
    text: &str,
    ts: &str,
    wait: Duration,
    own: &str,
    sync: impl FnOnce(&File) -> std::io::Result<()>,
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

    let lock = open_lock(inbox_dir, to).map_err(failed)?;
    let lock = take_lock(lock, own, wait).map_err(|e| match e {
        LockWait::Timeout => format!("the inbox lock for @{to} was held for {}s — nothing was appended", wait.as_secs()),
        LockWait::Io(e) => failed(e),
    })?;
    let written = append_line(&inbox_dir.join(format!("{to}.jsonl")), &line, sync);
    // The inbox is closed inside `append_line`; only now does the lock go.
    drop(lock);
    written.map_err(failed)
}

/// The inbox's `.lock` file, opened read-write like every lock descriptor the
/// shell opens: the Linux NFS client refuses a shared lock on one without read
/// access, so the rule has no exception here either.
fn open_lock(inbox_dir: &Path, to: &str) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .create(true)
        .append(true)
        .open(inbox_dir.join(format!("{to}.lock")))
}

enum LockWait {
    Timeout,
    Io(std::io::Error),
}

/// The exclusive lock on the inbox's `.lock` file within `wait`, chosen by the
/// lock manager `own`. The Linux NFSv4 client retries a blocked lock with a
/// backoff that doubles from 100 ms, so a local writer re-takes the lock before
/// a remote waiter's next retry and a blocking waiter can sleep past a free lock
/// and time out: under `nfs4 ` a non-blocking try is repeated every 15-25 ms
/// instead. NLM (v3) and one machine's own kernel lock (`local …`, `none@…`)
/// wake a blocked waiter on release, so those block, on a helper thread that
/// the caller bounds. A `flock` belongs to the open file description, so it
/// travels with the `File`.
fn take_lock(lock: File, own: &str, wait: Duration) -> Result<File, LockWait> {
    if own.starts_with("nfs4 ") {
        let deadline = Instant::now() + wait;
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(lock),
                Err(std::fs::TryLockError::Error(e)) => return Err(LockWait::Io(e)),
                Err(std::fs::TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return Err(LockWait::Timeout);
            }
            let jitter = 15 + u64::from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos())) % 11;
            std::thread::sleep(Duration::from_millis(jitter));
        }
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locked = lock.lock().map(|()| lock);
        // A send that fails means the caller gave up: the `File` comes back in
        // the error and is dropped at once, which releases the lock.
        let _ = tx.send(locked);
    });
    match rx.recv_timeout(wait) {
        Ok(Ok(lock)) => Ok(lock),
        Ok(Err(e)) => Err(LockWait::Io(e)),
        Err(_) => Err(LockWait::Timeout),
    }
}

/// `line` goes in whole or not at all ("filed" means kept): an unterminated
/// tail (a writer that died mid-line, or NULs after a client crash) is cut
/// back to the last newline first — everything past it was written by a
/// writer that never answered `filed`, so nothing kept is lost — the bytes are
/// flushed to disk on this descriptor by `sync` before `Ok`, and any error
/// cuts the file back to its length after that cut. That length is a seek to
/// the end of this descriptor, opened under the lock — never a size that an
/// attribute cache can answer stale.
/// Production passes `File::sync_data` as `sync`; a test passes one that
/// fails.
fn append_line(
    path: &Path,
    line: &str,
    sync: impl FnOnce(&File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut f: File = OpenOptions::new().create(true).read(true).append(true).open(path)?;
    let mut len = f.seek(SeekFrom::End(0))?;
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            let keep = end_of_last_line(&mut f, len)?;
            // Windows refuses to truncate through an append-only handle,
            // so the cut and the rollback below each open a write handle.
            OpenOptions::new().write(true).open(path)?.set_len(keep)?;
            tracing::warn!(
                "cut {} bytes of an unterminated line a dead writer left in {}",
                len - keep,
                path.display()
            );
            len = keep;
        }
    }
    let written = f.write_all(line.as_bytes()).and_then(|()| sync(&f));
    if written.is_err() {
        let _ = OpenOptions::new().write(true).open(path).and_then(|t| t.set_len(len));
    }
    written
}

/// The offset just past the last `\n` in the first `len` bytes of `f`, read
/// backwards in blocks; 0 when there is none.
fn end_of_last_line(f: &mut File, len: u64) -> std::io::Result<u64> {
    const BLOCK: u64 = 4096;
    let mut end = len;
    let mut buf = vec![0u8; BLOCK as usize];
    while end > 0 {
        let start = end.saturating_sub(BLOCK);
        let n = (end - start) as usize;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|&b| b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The lock record's name, beside `registry.json` — never in `inbox/`, and
/// nothing globs for it.
pub const LOCK_RECORD: &str = "inbox-lock-manager";

/// Local block filesystems: a lock on one is this kernel's own.
#[cfg(any(target_os = "linux", test))]
const LOCAL_FS: [&str; 7] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs"];

/// The record's text: line 1 the lock manager `id` appends to
/// `<comm_home>/inbox` go through, line 2 the writer's machine id — no line 2
/// when it has none, an unknown writer.
fn record_text(id: &str, mid: Option<&str>) -> String {
    mid.map_or_else(|| format!("{id}\n"), |m| format!("{id}\n{m}\n"))
}

/// The full record in a temp file beside it, fsynced. Each call its own name
/// (machine, pid, sequence), so two racing daemons never share one.
fn record_temp(comm_home: &Path, id: &str, mid: Option<&str>) -> std::io::Result<PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = comm_home.join(format!(".{LOCK_RECORD}.{}.{}.{n}", mid.unwrap_or("none"), std::process::id()));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    if let Err(e) = f.write_all(record_text(id, mid).as_bytes()).and_then(|()| f.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// The exclusive create: of two daemons starting at once only one makes the
/// record. A hard link of the finished temp file, so the record never exists
/// half-written. Returns the link's own answer: `Ok` when this call made the
/// record; on any error the caller's re-read decides. The temp file goes
/// either way.
pub fn create_lock_record(comm_home: &Path, id: &str, mid: Option<&str>) -> std::io::Result<()> {
    let tmp = record_temp(comm_home, id, mid)?;
    let linked = std::fs::hard_link(&tmp, comm_home.join(LOCK_RECORD));
    let _ = std::fs::remove_file(&tmp);
    linked
}

/// The only overwrite (a temp file, then a rename): the hub over a record its
/// own machine wrote for another lock manager — itself, before a remount.
fn replace_lock_record(comm_home: &Path, id: &str, mid: Option<&str>) -> std::io::Result<()> {
    let tmp = record_temp(comm_home, id, mid)?;
    std::fs::rename(&tmp, comm_home.join(LOCK_RECORD)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Who a daemon is to its comm folder, evaluated at each use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Writes the record and never forwards a filing.
    Hub,
    /// Files only on a proven shared lock; everything else goes to the hub.
    Guest,
}

/// Whether `dir` is on this box's own disk, asked of the folder on every OS
/// (a macOS home or a Windows drive can be a network share). Linux: `own_identity`,
/// the folder's lock identity, is `local <machine-id>`. macOS: its mount carries
/// `MNT_LOCAL`. Windows: a fixed drive whose canonical path is not UNC. Any other
/// OS, or any error: not own disk.
#[allow(unused_variables)]
pub fn own_disk(dir: &Path, own_identity: &str) -> bool {
    #[cfg(target_os = "linux")]
    return own_identity.starts_with("local ");
    #[cfg(target_os = "macos")]
    return matches!(crate::rows::spawn::state_root::macos_only::mounted_locally(dir), Ok(true));
    #[cfg(windows)]
    return windows_volume_fixed(dir).is_some_and(|(fixed, canonical)| windows_own_disk(fixed, &canonical));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return false;
}

/// The Windows decision, pure: a fixed drive whose canonical path is not UNC.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn windows_own_disk(fixed: bool, canonical: &str) -> bool {
    fixed && !is_unc(canonical)
}

/// `\\?\UNC\…`, or `\\…` that is not the `\\?\` or `\\.\` device prefix.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn is_unc(canonical: &str) -> bool {
    canonical.starts_with(r"\\?\UNC\")
        || (canonical.starts_with(r"\\") && !canonical.starts_with(r"\\?\") && !canonical.starts_with(r"\\.\"))
}

/// `(is a fixed drive, canonical path)` for `dir`, `None` on any error.
#[cfg(windows)]
fn windows_volume_fixed(dir: &Path) -> Option<(bool, String)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    // `DRIVE_FIXED` (winbase.h); windows-sys keeps it in a feature this crate does not otherwise use.
    const DRIVE_FIXED: u32 = 3;
    let canonical = std::fs::canonicalize(dir).ok()?.to_string_lossy().into_owned();
    // The volume root of the canonical path itself, so a volume mounted in a
    // folder is asked about itself, not about the drive that holds the folder.
    let path: Vec<u16> = std::ffi::OsStr::new(&canonical).encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; path.len() + 1];
    // SAFETY: `path` is nul-terminated and `root` holds `root.len()` units.
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    // SAFETY: `root` is a nul-terminated UTF-16 string.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    Some((kind == DRIVE_FIXED, canonical))
}

/// The record's two lines: the lock manager, and the machine that wrote it —
/// `None` when there is no second line, an unknown writer that is never "mine".
pub fn parse_record(text: &str) -> (&str, Option<&str>) {
    let mut lines = text.lines();
    let id = lines.next().unwrap_or("");
    (id, lines.next().filter(|w| !w.is_empty()))
}

/// What the hub does with the record when it starts.
#[derive(Debug, PartialEq, Eq)]
pub enum AtStart {
    /// Absent: made by the exclusive create.
    Create,
    /// Line 1 already names this daemon's lock manager; nothing is written.
    Current,
    /// This machine wrote it for another lock manager (the hub after a
    /// remount): replaced.
    Replace,
    /// Left untouched, and why.
    Keep(String),
}

/// Line 2 names this machine — never when either side has no machine id.
fn written_here(writer: Option<&str>, own_mid: Option<&str>) -> bool {
    writer.is_some() && writer == own_mid
}

/// The start-time decision from this daemon's lock manager `own` and machine
/// id `own_mid`: only the hub writes, a record another machine wrote is a
/// different lock manager and is kept, a guest never writes or deletes it,
/// and a hub with no machine id (bare `none`) never creates or replaces it.
pub fn at_start(role: Role, own: &str, own_mid: Option<&str>, record: Option<&str>, path: &Path) -> AtStart {
    if role == Role::Guest {
        return AtStart::Keep(format!(
            "{path}: this daemon is a guest on its folder's hub, which alone writes the inbox lock record",
            path = path.display()
        ));
    }
    if own == "none" {
        return AtStart::Keep(refusal::no_machine_id(path));
    }
    match record.map(parse_record) {
        None => AtStart::Create,
        Some((id, _)) if id == own => AtStart::Current,
        Some((_, writer)) if written_here(writer, own_mid) => AtStart::Replace,
        Some((id, writer)) => AtStart::Keep(refusal::foreign(id, writer, own, path)),
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

/// The route for one filing, from this daemon's own lock manager `own` and
/// machine id `own_mid` (recomputed per filing) and the record: local only
/// when line 1 is `own` and `own` is not bare `none`, and a hub with bare
/// `none` refuses every filing. A `none@<machine>` record carries its one
/// machine in line 1 itself, so only that machine's writers ever match it.
pub fn route(
    role: Role,
    own: &str,
    own_mid: Option<&str>,
    record: Option<&str>,
    forwarded: bool,
    self_host: &str,
    path: &Path,
) -> Route {
    let rec = record.map(parse_record);
    if own != "none" && rec.is_some_and(|(id, _)| id == own) {
        return Route::Local;
    }
    match (role, rec) {
        (Role::Guest, _) if forwarded => Route::Refuse(refusal::forwarded_to_guest(self_host)),
        (Role::Guest, _) => Route::Forward,
        (Role::Hub, _) if own == "none" => Route::Refuse(refusal::no_machine_id(path)),
        (Role::Hub, None) => Route::Refuse(refusal::no_record(path)),
        (Role::Hub, Some((id, writer))) if written_here(writer, own_mid) => Route::Refuse(refusal::remounted(own, id)),
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
            "the inbox lock record names {rec}, written by {}, a different lock manager from this hub's {own}: stop every daemon on this comm folder, delete {}, then start the hub",
            writer.map_or_else(|| "an unknown machine".to_string(), |w| format!("machine {w}")),
            path.display()
        )
    }

    pub fn no_machine_id(path: &Path) -> String {
        format!(
            "this hub has no machine id, so it cannot own the inbox lock record at {}: give this machine a machine id (/etc/machine-id on Linux), then restart the daemon",
            path.display()
        )
    }

    pub fn hosts_toml_unreadable(err: &str) -> String {
        format!("this box's hosts.toml cannot be read, so it names no hub: {err}")
    }

    pub fn forwarded_to_guest(self_host: &str) -> String {
        format!("a forwarded frame reached a daemon that is not its folder's hub ({self_host})")
    }

    pub fn hub_did_not_answer(endpoint: &str, err: &str) -> String {
        format!("the hub did not answer at {endpoint}: {err}")
    }
}

/// Test-only: runs between `record_at_start`'s first read and its link.
#[cfg(test)]
thread_local! {
    static BETWEEN_READ_AND_LINK: std::cell::RefCell<Option<Box<dyn Fn(&Path)>>> = std::cell::RefCell::new(None);
}

/// The hub's start step over this daemon's lock manager `own` for `inbox/`
/// and its machine id `own_mid` (the caller computes both, after creating
/// `inbox/`): decide, then create or replace. A create whose link fails, with
/// any error, re-reads the record once: a record it finds decides as at the
/// start — this daemon's own (a retried NFS link that made it) reads back
/// `Current`, another's is the lost race's answer — and when it finds none the
/// link's error is returned. Returns the role and what it did.
pub fn record_at_start(
    comm_home: &Path,
    topology_hub: bool,
    own: &str,
    own_mid: Option<&str>,
) -> std::io::Result<(Role, AtStart)> {
    // The hub when the topology says so (or names none) or the folder is on this box's own disk.
    let role = if topology_hub || own_disk(&comm_home.join("inbox"), own) { Role::Hub } else { Role::Guest };
    let path = comm_home.join(LOCK_RECORD);
    let record = std::fs::read_to_string(&path).ok();
    let mut decision = at_start(role, own, own_mid, record.as_deref(), &path);
    if decision == AtStart::Create {
        #[cfg(test)]
        BETWEEN_READ_AND_LINK.with(|h| h.borrow().as_ref().map(|f| f(&path)));
        if let Err(e) = create_lock_record(comm_home, own, own_mid) {
            let Ok(text) = std::fs::read_to_string(&path) else { return Err(e) };
            decision = at_start(role, own, own_mid, Some(&text), &path);
        }
    }
    if decision == AtStart::Replace {
        replace_lock_record(comm_home, own, own_mid)?;
    }
    Ok((role, decision))
}

/// This machine's id: the record's line 2 and the `@` of `none@…`. Linux
/// `/etc/machine-id`; macOS `gethostuuid(2)`; Windows the registry's
/// `MachineGuid`; any other OS, or any error, none. Never a hostname, which
/// two machines can share.
pub fn machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    let id = std::fs::read_to_string("/etc/machine-id").ok().map(|m| m.trim().to_string());
    #[cfg(target_os = "macos")]
    let id = macos_host_uuid();
    #[cfg(windows)]
    let id = windows_machine_guid();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let id: Option<String> = None;
    id.filter(|m| !m.is_empty())
}

/// `gethostuuid(2)`, waiting at most a second.
#[cfg(target_os = "macos")]
fn macos_host_uuid() -> Option<String> {
    let mut b = [0u8; 16];
    let wait = libc::timespec { tv_sec: 1, tv_nsec: 0 };
    // SAFETY: `b` is the 16-byte `uuid_t` the call fills.
    (unsafe { libc::gethostuuid(b.as_mut_ptr(), &wait) } == 0).then(|| uuid_text(&b))
}

/// A UUID's 16 bytes as its canonical text, uppercase and hyphenated.
#[cfg(any(target_os = "macos", test))]
fn uuid_text(b: &[u8; 16]) -> String {
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// `HKLM\SOFTWARE\Microsoft\Cryptography`'s `MachineGuid`, from the 64-bit view.
#[cfg(windows)]
fn windows_machine_guid() -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (key, value) = (wide(r"SOFTWARE\Microsoft\Cryptography"), wide("MachineGuid"));
    let mut buf = [0u16; 64];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `key` and `value` are nul-terminated, and `buf` holds `bytes` bytes.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    let units = buf.get(..bytes as usize / 2).filter(|_| err == 0)?;
    String::from_utf16(units).ok().map(|s| s.trim_end_matches('\0').to_string())
}

/// The lock manager an append to `dir` goes through: `nfs4 <source>` (an NFS
/// v4 mount with `local_lock=none`), `local <machine-id>`, or, for anything
/// whose lock is not provably one manager's — every non-Linux platform
/// included — `none@<machine-id>`, this machine's own lock alone (bare `none`
/// with no machine id, which never matches). `comm-lib.sh`'s
/// `sot_inbox_lock_identity` computes the same string with `findmnt -T`.
pub fn lock_identity(dir: &Path) -> String {
    let mid = machine_id().unwrap_or_default();
    #[cfg(target_os = "linux")]
    {
        if let (Ok(path), Ok(mountinfo)) = (
            std::fs::canonicalize(dir),
            std::fs::read_to_string("/proc/self/mountinfo"),
        ) {
            return identity_from(&mountinfo, &path, &mid);
        }
    }
    let _ = dir;
    unknown_lock(&mid)
}

/// An unknown lock is its one machine's: `none@<machine-id>`, or bare `none`.
fn unknown_lock(machine_id: &str) -> String {
    if machine_id.is_empty() { "none".into() } else { format!("none@{machine_id}") }
}

/// `lock_identity` over mountinfo text, for a canonical `path`. The mount is
/// the one `findmnt -T` finds: the longest mount-point prefix, the LAST entry
/// when one is stacked over another, `\040`-style escapes decoded. An `nfs4`
/// mount counts only with `vers=4.x` and `local_lock=none` in its super options.
#[cfg(any(target_os = "linux", test))]
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
        _ => unknown_lock(machine_id),
    }
}

/// Decode mountinfo's `\ooo` octal escapes (a space is `\040`).
#[cfg(any(target_os = "linux", test))]
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
#[path = "inbox_tests.rs"]
mod tests;
