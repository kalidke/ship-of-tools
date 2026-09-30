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

    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(inbox_dir.join(format!("{to}.lock")))
        .map_err(failed)?;
    let lock = take_lock(lock, own, wait).map_err(|e| match e {
        LockWait::Timeout => format!("the inbox lock for @{to} was held for {}s — nothing was appended", wait.as_secs()),
        LockWait::Io(e) => failed(e),
    })?;
    let written = append_line(&inbox_dir.join(format!("{to}.jsonl")), &line, sync);
    // The inbox is closed inside `append_line`; only now does the lock go.
    drop(lock);
    written.map_err(failed)
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
            f.set_len(keep)?;
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
        let _ = f.set_len(len);
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
/// half-written; `Ok(false)` when another writer's already stood there. On
/// NFS a link can report an error though it succeeded (a retried request), so
/// a temp file with two links is this call's record. The temp file goes
/// either way.
pub fn create_lock_record(comm_home: &Path, id: &str, mid: Option<&str>) -> std::io::Result<bool> {
    let tmp = record_temp(comm_home, id, mid)?;
    let made = match std::fs::hard_link(&tmp, comm_home.join(LOCK_RECORD)) {
        Ok(()) => Ok(true),
        Err(_) if links(&tmp) == Some(2) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    made
}

/// A file's link count, where the OS reports one.
fn links(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    return std::fs::metadata(p).ok().map(|m| std::os::unix::fs::MetadataExt::nlink(&m));
    #[cfg(not(unix))]
    {
        let _ = p;
        None
    }
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
    return matches!(crate::capsule_workspace::macos_only::mounted_locally(dir), Ok(true));
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
/// different lock manager and is kept, and a guest never writes or deletes it.
pub fn at_start(role: Role, own: &str, own_mid: Option<&str>, record: Option<&str>, path: &Path) -> AtStart {
    if role == Role::Guest {
        return AtStart::Keep(format!(
            "{path}: this daemon is a guest on its folder's hub, which alone writes the inbox lock record",
            path = path.display()
        ));
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
/// when line 1 is `own` and `own` is not bare `none`. A `none@<machine>`
/// record carries its one machine in line 1 itself, so only that machine's
/// writers ever match it.
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

/// The hub's start step over this daemon's lock manager `own` for `inbox/`
/// and its machine id `own_mid` (the caller computes both, after creating
/// `inbox/`): decide, then create or replace. A lost create race re-reads the
/// winner's record and decides over it. Returns the role and what it did.
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
    if decision == AtStart::Create && !create_lock_record(comm_home, own, own_mid)? {
        decision = at_start(role, own, own_mid, Some(&std::fs::read_to_string(&path)?), &path);
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

    // The stubbed fsync failure, Rust arm: `sync` starts a reader, waits 0.5 s
    // and fails, so `append_line` cuts the in-flight line back. The reader is
    // the scripts' own (comm-lib.sh), so the guards under test are the real
    // ones. A comm home whose folder record matches this disk lets the locked
    // reader take the shared lock; a PATH with no flock(1) leaves the other
    // one unlocked.
    #[cfg(target_os = "linux")]
    fn failing_sync_with_reader(
        home: &Path,
        path: &str,
        reader: &'static str,
    ) -> (String, String) {
        let inbox = home.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let id = shell(home, &std::env::var("PATH").unwrap(), r#"sot_inbox_lock_identity "$INBOX_DIR""#);
        std::fs::write(home.join("inbox-lock-manager"), &id.stdout).unwrap();
        file_frame(&inbox, "a", "h", false, "one", "t", Duration::from_secs(5), "local t").unwrap();
        let child = std::cell::RefCell::new(None);
        let sync = |_f: &File| -> std::io::Result<()> {
            let c = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!("source {}; {reader}", scripts_lib()))
                .env("SOT_COMM_HOME", home)
                .env("PATH", path)
                .env("SOT_INBOX_READ_WAIT_SECS", "5")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            *child.borrow_mut() = Some(c);
            std::thread::sleep(Duration::from_millis(500));
            Err(std::io::Error::other("stubbed fsync failure"))
        };
        let r = file_frame_with(&inbox, "a", "h", false, "inflight", "t", Duration::from_secs(5), "local t", sync);
        assert!(r.unwrap_err().contains("stubbed fsync failure"));
        assert_eq!(read_lines(&inbox.join("h.jsonl")).len(), 1, "the in-flight line was not cut back");
        let out = child.into_inner().unwrap().wait_with_output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
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
        );
        assert_eq!(out.trim(), "2", "the unlocked reader should have counted the in-flight line");
        file_frame(&d.path().join("inbox"), "a", "h", false, "real", "t", Duration::from_secs(5), "local t").unwrap();
        let o = shell(d.path(), np, r#"sot_cursor_offset h"#);
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "1");
        assert!(String::from_utf8_lossy(&o.stderr)
            .contains("the last line read from @h's inbox was cut back; reading from the line before it"));
        assert_eq!(read_lines(&d.path().join("inbox/h.jsonl"))[1]["msg"], "real");
    }

    // A waiter that gave up never keeps the lock.
    #[test]
    fn a_waiter_that_gave_up_never_keeps_the_lock() {
        let d = tempfile::tempdir().unwrap();
        let holder = OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.path().join("h.lock"))
            .unwrap();
        holder.lock().unwrap();
        let w = Duration::from_millis(200);
        assert!(file_frame(d.path(), "a", "h", false, "late", "t", w, "local t").is_err());
        drop(holder);
        let start = std::time::Instant::now();
        file_frame(d.path(), "a", "h", false, "only", "t", w, "local t").unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        let lines = read_lines(&d.path().join("h.jsonl"));
        assert_eq!((lines.len(), lines[0]["msg"].as_str()), (1, Some("only")));
    }

    // A holder frees the lock after 300 ms; the filing follows it within a
    // retry, under either wait, and a lock that is never freed gives the same
    // sentence at the bound.
    fn held_then_freed(own: &str) -> (Duration, Result<(), String>) {
        let d = tempfile::tempdir().unwrap();
        let holder = OpenOptions::new().create(true).append(true).open(d.path().join("h.lock")).unwrap();
        holder.lock().unwrap();
        let freed = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(holder);
        });
        let start = Instant::now();
        let r = file_frame(d.path(), "a", "h", false, "x", "t", Duration::from_secs(5), own);
        freed.join().unwrap();
        (start.elapsed(), r)
    }

    #[test]
    fn under_nfs4_the_lock_is_polled_and_follows_the_unlock() {
        let (took, r) = held_then_freed("nfs4 srv:/export");
        r.unwrap();
        assert!(took >= Duration::from_millis(300) && took < Duration::from_millis(500), "{took:?}");
    }

    #[test]
    fn under_a_local_lock_the_wait_blocks_and_follows_the_unlock() {
        for own in ["local m", "none@m"] {
            let (took, r) = held_then_freed(own);
            r.unwrap();
            assert!(took >= Duration::from_millis(300) && took < Duration::from_millis(500), "{own}: {took:?}");
        }
    }

    #[test]
    fn both_waits_give_the_same_sentence_at_the_bound() {
        let d = tempfile::tempdir().unwrap();
        let holder = OpenOptions::new().create(true).append(true).open(d.path().join("h.lock")).unwrap();
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
}
