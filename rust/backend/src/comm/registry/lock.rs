//! The sot-comm registry lock, the daemon's arm (B1b). `comm-lib.sh`'s
//! `with_lock` is the other arm, and its header is the full account; the two
//! take one lock with one record, and a parity test holds the records
//! byte-equal.
//!
//! The lock is the FILE `<comm home>/.registry.lock`, one line naming its
//! holder, `name:machine:boot:pidns:pid:start`, made by `link(2)` of a temp
//! file that already holds that line. A holder is proved dead on Linux only,
//! and only from its own machine; nothing else is ever forced, so a waiter
//! fails closed at its bound naming the holder. A waiter that proves the
//! holder D dead takes the marker `.registry.lock.reclaim.<D>`, settles, reads
//! the lock again fresh, and removes it only if it still names D. A holder that
//! releases, exits and is reaped between a waiter's read and its proof is
//! proved dead too; its marker stays, and the re-read leaves the lock alone. Markers are
//! kept forever, but for the one this daemon takes for a lock naming itself,
//! which it removes when that reclaim removes the lock or finds it gone. File
//! names map ':' to '.', because
//! Windows reads a ':' in a name as a stream; the record keeps its colons.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);

/// After taking a marker: a dead shell holder's orphaned `jq`/`mv`/`rm`, and a
/// dead holder's in-flight calls, land within it.
const SETTLE: Duration = Duration::from_secs(1);

/// One thread of this process at a time is inside the lock protocol, from
/// `acquire` until its `Held` drops (review S1): its threads share one ID, so
/// without this a marker one thread abandoned would read as live to them all.
static TURN: Mutex<()> = Mutex::new(());

/// The lock, held; dropping it (a panic unwind included) removes the file and
/// then gives up the turn, so a panicking critical section leaves neither
/// behind.
pub struct Held {
    path: PathBuf,
    _turn: MutexGuard<'static, ()>,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Take `lock`, waiting up to `bound`. `Err` is the one FAILED line naming the
/// holder and the recovery. The reclaim runs on the first failed take, before
/// any sleep; a removed lock is retaken at once, as part of the try that
/// removed it, even past the deadline.
pub fn acquire(lock: &Path, bound: Duration) -> Result<Held, String> {
    acquire_with(lock, bound, SETTLE)
}

/// `bound` is a deadline over the wait for the turn and the lock together,
/// checked where comm/PROTOCOL.md's Bounds says.
fn acquire_with(lock: &Path, bound: Duration, settle: Duration) -> Result<Held, String> {
    let deadline = Instant::now() + bound;
    let turn = loop {
        match TURN.try_lock() {
            Ok(turn) => break turn,
            Err(TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(TryLockError::WouldBlock) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "registry lock {} was not tried: another thread of this daemon held its turn past the bound",
                lock.display()
            ));
        }
        std::thread::sleep(POLL.min(deadline - now));
    };
    let me = Me::now();
    let mut last = Blocked { holder: None, who: None, why: "it was released just now".into(), by_hand: false, gone: false };
    let (mut first, mut retook) = (true, false);
    loop {
        match take(lock, lock, &me.id) {
            Ok(true) => return Ok(Held { path: lock.to_path_buf(), _turn: turn }),
            Ok(false) => {}
            Err(e) => return Err(format!("registry lock {} cannot be taken: {e}", lock.display())),
        }
        // A failed retake: the FAILED line names whoever took it.
        if std::mem::take(&mut retook) {
            #[cfg(all(test, target_os = "linux"))]
            let _ = tests::GONE_AFTER_RETAKE.swap(false, std::sync::atomic::Ordering::SeqCst).then(|| fs::remove_file(lock));
            last.holder = fresh(lock, me.mine.is_some()).ok();
        }
        if !first && Instant::now() >= deadline {
            return Err(last.fail_text(lock));
        }
        first = false;
        match step(lock, &me, settle, deadline) {
            Ok(()) => {
                #[cfg(all(test, target_os = "linux"))]
                let _ = tests::AFTER_STEP.lock().unwrap().take().map(|d2| fs::write(lock, d2));
                last = Blocked { holder: None, who: None, why: RETAKEN.into(), by_hand: false, gone: false };
                retook = true;
                continue;
            }
            Err(Some(b)) => last = b,
            Err(None) => last.released(),
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        if Instant::now() >= deadline {
            return Err(last.fail_text(lock));
        }
    }
}

/// Why a retake after a reclaim removed the lock failed.
const RETAKEN: &str = "another process took it as soon as a dead holder's lock was removed";

/// This process's record, and what it proves with: `name:machine:boot:pidns`
/// where it can prove a death (Linux, with a `/proc` that is its own).
struct Me {
    id: String,
    mine: Option<[String; 4]>,
}

impl Me {
    fn now() -> Me {
        let pid = std::process::id();
        if !proc_is_mine(pid) {
            return Me { id: format!("{}:-:-:-:{pid}:-", host_field()), mine: None };
        }
        let id = record_for(pid);
        let f: Vec<&str> = id.split(':').collect();
        let mine = [f[0], f[1], f[2], f[3]].map(str::to_string);
        Me { id, mine: Some(mine) }
    }

    /// `x` is this process only where its ID carries proof: without it the ID
    /// is `host:-:-:-:pid:-`, and `-` never equals anything, so another box's
    /// process with the same host name and pid would read as mine (review SF1).
    fn is_me(&self, x: &str) -> bool {
        self.mine.is_some() && x == self.id
    }
}

/// `sot_host`, every char outside `[A-Za-z0-9._-]` made `_`; `-` if none.
fn host_field() -> String {
    let n: String = sot_log::host::state_dir::host_name()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect();
    if n.is_empty() { "-".into() } else { n }
}

/// The record for `pid` on this machine, as `_sot_lock_self_id` writes it.
fn record_for(pid: u32) -> String {
    let [machine, boot, pidns, start] = proof_fields(pid);
    format!("{}:{machine}:{boot}:{pidns}:{pid}:{start}", host_field())
}

#[cfg(target_os = "linux")]
fn proc_is_mine(pid: u32) -> bool {
    fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(|f| f == pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn proc_is_mine(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn proof_fields(pid: u32) -> [String; 4] {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .filter(|s| !s.is_empty());
    let pidns = fs::read_link(format!("/proc/{pid}/ns/pid")).ok().and_then(|l| {
        Some(l.to_str()?.strip_prefix("pid:[")?.strip_suffix(']')?.to_string())
    });
    let start = sot_log::identity::challenge_unix::process_start_ticks(pid).ok().map(|t| t.to_string());
    [crate::comm::mail::inbox::machine_id(), boot, pidns, start]
        .map(|f| f.unwrap_or_else(|| "-".into()))
}

#[cfg(not(target_os = "linux"))]
fn proof_fields(_pid: u32) -> [String; 4] {
    ["-", "-", "-", "-"].map(str::to_string)
}

/// Six colon fields with a numeric pid, or `None` (unreadable).
fn parse(line: &str) -> Option<Vec<&str>> {
    let f: Vec<&str> = line.split(':').collect();
    (f.len() == 6 && !f[4].is_empty() && f[4].bytes().all(|b| b.is_ascii_digit())).then_some(f)
}

/// One attempt to create `target` holding `id`: `Ok(true)` taken, `Ok(false)`
/// held, `Err` anything else. `lock` names the folder and the temp file.
fn take(lock: &Path, target: &Path, id: &str) -> Result<bool, String> {
    let tmp = PathBuf::from(format!("{}.tmp.{}", lock.display(), id.replace(':', ".")));
    // An earlier temp is removed first: one a take could not remove may still
    // be a link to that take's marker, and `create_new` refuses one that stays.
    let _ = fs::remove_file(&tmp);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(format!("{id}\n").as_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    let linked = fs::hard_link(&tmp, target);
    // A retransmitted LINK on NFSv3 answers "exists" for this call's own link,
    // so my temp file's link count, read through an open, decides.
    let taken = linked.is_ok() || fs::File::open(&tmp).and_then(|f| f.metadata()).map(|m| nlink(&m) == 2).unwrap_or(false);
    let _ = fs::remove_file(&tmp);
    match linked {
        _ if taken => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("cannot hard-link in {}: {e}", target.parent().unwrap_or(target).display())),
        Ok(()) => unreachable!("a made link is taken"),
    }
}

#[cfg(unix)]
fn nlink(m: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(m)
}

#[cfg(not(unix))]
fn nlink(_m: &fs::Metadata) -> u64 {
    0
}

/// `path`'s record, read fresh: opening the folder first forces its GETATTR
/// (close-to-open), so a changed folder drops every cached lookup beneath it
/// and the record's own open goes to the server. The folder open is required
/// only where a death can be proved. `Err(Some)` carries what was read whole
/// when it is not a record, empty included; `Err(None)` = not read (the folder
/// open required and failed, or the file's read failed).
fn fresh(path: &Path, proves: bool) -> Result<String, Option<String>> {
    let dir_ok = path.parent().is_some_and(|d| fs::File::open(d).is_ok());
    if proves && !dir_ok {
        return Err(None);
    }
    let raw = fs::read(path).map_err(|_| None)?;
    let line = String::from_utf8_lossy(&raw).lines().next().unwrap_or("").to_string();
    if parse(&line).is_some() { Ok(line) } else { Err(Some(line)) }
}

enum Verdict {
    Dead,
    Alive,
    Unprovable,
}

fn judge(id: &str, me: &Me) -> (Verdict, &'static str) {
    let Some(mine) = &me.mine else {
        return (Verdict::Unprovable, "this box cannot prove a death");
    };
    let f = parse(id).unwrap_or_default();
    let [name, machine, boot, pidns, pid, start] = [0, 1, 2, 3, 4, 5].map(|i| f.get(i).copied().unwrap_or("-"));
    let [me_name, me_machine, me_boot, me_pidns] = mine.each_ref().map(String::as_str);
    let known = |s: &str| s != "-";
    if known(boot) && known(pidns) && boot == me_boot && pidns == me_pidns {
        match pid.parse::<u32>().map_err(|_| None).and_then(|p| proc_start(p).map_err(Some)) {
            Ok(t) if known(start) && t != start => (Verdict::Dead, "its pid now names another process"),
            Ok(_) => (Verdict::Alive, "it is running"),
            Err(Some(e)) if e.kind() == std::io::ErrorKind::NotFound => (Verdict::Dead, "it has exited"),
            Err(_) => (Verdict::Unprovable, "its /proc entry cannot be read"),
        }
    } else if known(machine) && known(boot) && known(me_boot) && known(name)
        && machine == me_machine && name == me_name && boot != me_boot
    {
        (Verdict::Dead, "its machine has rebooted since")
    } else if !known(machine) || machine != me_machine || name != me_name {
        (Verdict::Unprovable, "it is on another machine")
    } else {
        (Verdict::Unprovable, "it is in another pid namespace")
    }
}

#[cfg(target_os = "linux")]
fn proc_start(pid: u32) -> std::io::Result<String> {
    sot_log::identity::challenge_unix::process_start_ticks(pid).map(|t| t.to_string())
}

#[cfg(not(target_os = "linux"))]
fn proc_start(_pid: u32) -> std::io::Result<String> {
    Err(std::io::Error::other("no /proc"))
}

/// The comm home's mount makes the fresh read fresh: a local filesystem, or
/// NFS without `nocto`. `findmnt -T`'s mount: the longest mount-point prefix,
/// the last when stacked.
#[cfg(target_os = "linux")]
fn vouch(lock: &Path) -> Result<(), String> {
    let path = lock.parent().and_then(|d| fs::canonicalize(d).ok()).unwrap_or_default();
    let info = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut best: Option<(usize, String, String)> = None;
    for line in info.lines() {
        let Some((pre, post)) = line.split_once(" - ") else { continue };
        let pre: Vec<&str> = pre.split(' ').collect();
        let post: Vec<&str> = post.split(' ').collect();
        let (Some(point), Some(vfs_opts), Some(fstype), Some(fs_opts)) = (pre.get(4), pre.get(5), post.first(), post.get(2))
        else { continue };
        let point = PathBuf::from(point.replace("\\040", " "));
        let depth = point.components().count();
        if path.starts_with(&point) && best.as_ref().map_or(true, |b| depth >= b.0) {
            best = Some((depth, fstype.to_string(), format!("{vfs_opts},{fs_opts}")));
        }
    }
    let (_, fstype, opts) = best.unwrap_or_default();
    if !["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "tmpfs", "nfs", "nfs4"].contains(&fstype.as_str()) {
        let fstype = if fstype.is_empty() { "unknown" } else { &fstype };
        return Err(format!("the comm home's filesystem ({fstype}) does not prove a fresh read"));
    }
    if opts.split(',').any(|o| o == "nocto") {
        return Err("the comm home is mounted nocto, so no read of it is proved fresh".into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn vouch(_lock: &Path) -> Result<(), String> {
    Ok(())
}

/// Why a step did not free the lock: the ID the lock names (`None` for none),
/// the ID that blocks, the reason, and whether no reclaim can clear it, so only
/// a person can, by hand: a directory, or a record read whole that does not parse.
struct Blocked {
    holder: Option<String>,
    who: Option<String>,
    why: String,
    by_hand: bool,
    /// The lock was gone, or its record not read, at the last step: the holder
    /// is one last read, not one now.
    gone: bool,
}

impl Blocked {
    /// The lock was gone at the step, or its record could not be read: the
    /// by-hand flag goes, the holder stays named, and with none the reason says
    /// the record was not read.
    fn released(&mut self) {
        self.by_hand = false;
        self.gone = true;
        if self.holder.is_none() {
            self.why = "its record could not be read, so it may have been released since".into();
        }
    }

    fn fail_text(&self, lock: &Path) -> String {
        let age = fs::metadata(lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|t| Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64 - t.as_secs() as i64))
            .map_or("unknown".to_string(), |s| format!("{s}s"));
        let p = lock.display();
        if self.by_hand {
            return format!(
                "registry lock {p} still held ({age} old): {}. If its holder is dead, remove {p} by hand and retry.",
                self.why
            );
        }
        let Some(holder) = &self.holder else {
            return format!("registry lock {p} was not taken by the deadline: {}. Retry.", self.why);
        };
        let f = parse(holder).unwrap_or_default();
        if self.gone {
            return format!(
                "registry lock {p} was held by {} pid {} start {} when last read ({}); it may have been released since. Retry.",
                f.first().unwrap_or(&"-"),
                f.get(4).unwrap_or(&"-"),
                f.get(5).unwrap_or(&"-"),
                self.why,
            );
        }
        let who = self.who.as_deref().unwrap_or(holder);
        format!(
            "registry lock {p} is held by {} pid {} start {} ({age} old): {}. If it is dead, run any comm command on {}, or run comm-registry-lock-clear.sh.",
            f.first().unwrap_or(&"-"),
            f.get(4).unwrap_or(&"-"),
            f.get(5).unwrap_or(&"-"),
            self.why,
            who.split(':').next().unwrap_or("-"),
        )
    }
}

/// One reclaim attempt against the lock as it stands (`_sot_lock_step`):
/// `Ok` = this step saw the lock go, retake at once; `Err(None)` = released
/// since the take, or its record could not be read, a try that keeps the last
/// holder named, clears the by-hand flag and makes the text say "was held".
/// By hand: a directory, or a record read whole that does not parse. Every
/// step past a marker needs its creator proved dead, so the live process
/// holding the chain's last marker is the only one with authority over "the
/// lock names a member of the chain". A marker naming a record the chain
/// already holds, not mine, ends the walk: no reclaim can pass it (this
/// daemon's own marker, left naming it when it died during its reclaim; review
/// B1), so the lock is removed by hand. Past its first marker the walk stops
/// at `deadline`.
fn step(lock: &Path, me: &Me, settle: Duration, deadline: Instant) -> Result<(), Option<Blocked>> {
    let proves = me.mine.is_some();
    let blocked = |holder: Option<&String>, who: Option<&String>, why: String| {
        Err(Some(Blocked { holder: holder.cloned(), who: who.cloned(), why, by_hand: false, gone: false }))
    };
    let only_by_hand = |why: String| Err(Some(Blocked { holder: None, who: None, why, by_hand: true, gone: false }));
    let d = fresh(lock, proves);
    #[cfg(all(test, target_os = "linux"))]
    let d = if tests::FAIL_FIRST_READ.swap(false, std::sync::atomic::Ordering::SeqCst) { Err(None) } else { d };
    let d = match d {
        Ok(d) => d,
        // First, as a directory's read fails too.
        Err(_) if lock.is_dir() => return only_by_hand("held by an older version that records no holder".into()),
        // Not read: released, whether or not the lock exists now, as a live
        // writer can link it between the failed read and any existence test.
        Err(None) => return Err(None),
        Err(Some(read)) => {
            let read = if read.is_empty() { "empty".to_string() } else { read };
            return only_by_hand(format!("its record names no holder ({read})"));
        }
    };
    let mut chain = vec![d.clone()];
    loop {
        let x = chain.last().unwrap().clone();
        // A marker naming this process, seen by the thread holding the turn,
        // can only be one a thread of it abandoned (its re-read after the
        // settle failed), so it is adopted, as the shell adopts its own. One
        // naming this process while another of its threads is inside would be
        // live, and with the turn no thread inside the protocol sees that.
        if chain.len() > 1 && me.is_me(&x) {
            break;
        }
        if chain.len() > 1 && Instant::now() >= deadline {
            return blocked(Some(&d), Some(&x), "its reclaim chain was still being walked at the deadline".into());
        }
        // The lock itself naming this process, seen by the thread holding the
        // turn, is one a thread of it left: its release's unlink failed, or
        // its own link went unconfirmed. `Held` removes the file before it
        // gives up the turn, so no live thread of it is inside; it goes
        // through the marker like any dead holder (review SF1).
        let (verdict, why) = if me.is_me(&x) {
            (Verdict::Dead, "it is this daemon's own, left by one of its threads")
        } else {
            judge(&x, me)
        };
        if !matches!(verdict, Verdict::Dead) {
            let why = if x == d {
                why.to_string()
            } else {
                let f = parse(&x).unwrap_or_default();
                format!("it is dead, but its reclaim by {} pid {} did not finish: {why}", f[0], f[4])
            };
            return blocked(Some(&d), Some(&x), why);
        }
        if chain.len() == 1 {
            if let Err(why) = vouch(lock) {
                return blocked(Some(&d), Some(&x), why);
            }
        }
        let marker = marker_for(lock, &x);
        match take(lock, &marker, &me.id) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => return blocked(Some(&d), Some(&x), format!("its reclaim marker {} cannot be read ({e})", marker.display())),
        }
        match fresh(&marker, proves) {
            Ok(r) if chain.contains(&r) && !me.is_me(&r) => {
                let why = format!("its reclaim marker {} names {r}, which its reclaim chain already holds, so no reclaim can pass it", marker.display());
                return only_by_hand(why);
            }
            Ok(r) => chain.push(r),
            Err(_) => return blocked(Some(&d), Some(&x), format!("its reclaim marker {} cannot be read", marker.display())),
        }
    }
    std::thread::sleep(settle);
    #[cfg(all(test, target_os = "linux"))]
    let _ = tests::GONE_IN_SETTLE.swap(false, std::sync::atomic::Ordering::SeqCst).then(|| fs::remove_file(lock));
    let reread = fresh(lock, proves);
    #[cfg(all(test, target_os = "linux"))]
    let reread = if tests::FAIL_REREAD.swap(false, std::sync::atomic::Ordering::SeqCst) { Err(None) } else { reread };
    let result = match reread {
        Ok(now) if chain.contains(&now) => {
            let _ = fs::remove_file(lock);
            Ok(())
        }
        Ok(now) => blocked(Some(&now), None, "it was taken again during the reclaim".into()),
        Err(_) if fs::symlink_metadata(lock).is_err() => Ok(()),
        Err(_) => blocked(Some(&d), None, "its record changed during the reclaim".into()),
    };
    // My own marker, naming me, once the lock is gone, removed or found
    // removed: a later death of this daemon with its lock named is reclaimed
    // like any holder's.
    if result.is_ok() && chain.iter().all(|r| me.is_me(r)) {
        let _ = fs::remove_file(marker_for(lock, &chain[0]));
    }
    result
}

/// The marker `reclaim.<x>` beside `lock`.
fn marker_for(lock: &Path, x: &str) -> PathBuf {
    PathBuf::from(format!("{}.reclaim.{}", lock.display(), x.replace(':', ".")))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "lock_tests.rs"]
mod tests;
