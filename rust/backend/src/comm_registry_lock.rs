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
//! the lock again fresh, and removes it only if it still names D. Markers are
//! kept forever. File names map ':' to '.', because Windows reads a ':' in a
//! name as a stream; the record keeps its colons.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);

/// After taking a marker: a dead shell holder's orphaned `jq`/`mv`/`rm`, and a
/// dead holder's in-flight calls, land within it.
const SETTLE: Duration = Duration::from_secs(1);

/// Keeps concurrent takes by this process's threads on distinct temp files.
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// The lock, held; dropping it (a panic unwind included) removes the file, so
/// a panicking critical section never leaves the lock behind.
pub struct Held {
    path: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Take `lock`, waiting up to `bound`. `Err` is the one FAILED line naming the
/// holder and the recovery. The reclaim runs on the first failed take, before
/// any sleep; a removed lock is retaken at once and the settle is not a try.
pub fn acquire(lock: &Path, bound: Duration) -> Result<Held, String> {
    acquire_with(lock, bound, SETTLE)
}

fn acquire_with(lock: &Path, bound: Duration, settle: Duration) -> Result<Held, String> {
    let me = Me::now();
    let max_tries = (bound.as_millis() / POLL.as_millis()) as u64;
    let mut tries = 0;
    let mut last = Blocked { holder: None, who: None, why: "it was released just now".into() };
    loop {
        match take(lock, lock, &me.id) {
            Ok(true) => return Ok(Held { path: lock.to_path_buf() }),
            Ok(false) => {}
            Err(e) => return Err(format!("registry lock {} cannot be taken: {e}", lock.display())),
        }
        match step(lock, &me, settle) {
            Ok(()) => continue,
            Err(Some(b)) => last = b,
            Err(None) => {}
        }
        tries += 1;
        if tries > max_tries {
            return Err(last.fail_text(lock));
        }
        std::thread::sleep(POLL);
    }
}

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
}

/// `sot_host`, every char outside `[A-Za-z0-9._-]` made `_`; `-` if none.
fn host_field() -> String {
    let n: String = sot_log::state_dir::host_name()
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
    let first_line = |p: &str| {
        fs::read_to_string(p).ok().and_then(|s| s.lines().next().map(str::to_string)).filter(|s| !s.is_empty())
    };
    let pidns = fs::read_link(format!("/proc/{pid}/ns/pid")).ok().and_then(|l| {
        Some(l.to_str()?.strip_prefix("pid:[")?.strip_suffix(']')?.to_string())
    });
    let start = sot_log::challenge_unix::process_start_ticks(pid).ok().map(|t| t.to_string());
    [first_line("/etc/machine-id"), first_line("/proc/sys/kernel/random/boot_id"), pidns, start]
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
    let n = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let tmp = PathBuf::from(format!("{}.tmp.{}.{n}", lock.display(), id.replace(':', ".")));
    fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(format!("{id}\n").as_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    let linked = fs::hard_link(&tmp, target);
    // A retransmitted LINK on NFSv3 answers "exists" for this call's own link,
    // so my temp file's link count, read through an open, decides; never "the
    // target names my ID", which this process's threads share.
    let taken = linked.is_ok() || fs::File::open(&tmp).and_then(|f| f.metadata()).map(|m| nlink(&m) == 2).unwrap_or(false);
    let _ = fs::remove_file(&tmp);
    match linked {
        _ if taken => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.to_string()),
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
/// only where a death can be proved. `Err` carries what was read.
fn fresh(path: &Path, proves: bool) -> Result<String, String> {
    let dir_ok = path.parent().is_some_and(|d| fs::File::open(d).is_ok());
    if proves && !dir_ok {
        return Err(String::new());
    }
    let raw = fs::read(path).unwrap_or_default();
    let line = String::from_utf8_lossy(&raw).lines().next().unwrap_or("").to_string();
    if parse(&line).is_some() { Ok(line) } else { Err(line) }
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
    sot_log::challenge_unix::process_start_ticks(pid).map(|t| t.to_string())
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

/// Why a step did not free the lock: the ID the lock names (`None` for no
/// readable holder), the ID that blocks, and the reason.
struct Blocked {
    holder: Option<String>,
    who: Option<String>,
    why: String,
}

impl Blocked {
    fn fail_text(&self, lock: &Path) -> String {
        let age = fs::metadata(lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|t| Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64 - t.as_secs() as i64))
            .map_or("unknown".to_string(), |s| format!("{s}s"));
        let p = lock.display();
        let Some(holder) = &self.holder else {
            return format!(
                "registry lock {p} still held ({age} old): {}. If its holder is dead, remove {p} by hand and retry.",
                self.why
            );
        };
        let f = parse(holder).unwrap_or_default();
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
/// since the take, a try that keeps the last holder named. Every step past a marker needs
/// its creator proved dead, so the live process holding the chain's last
/// marker is the only one with authority over "the lock names a member of the
/// chain". A marker naming this process is another thread's, still at work.
fn step(lock: &Path, me: &Me, settle: Duration) -> Result<(), Option<Blocked>> {
    let proves = me.mine.is_some();
    let blocked = |holder: Option<&String>, who: Option<&String>, why: String| {
        Err(Some(Blocked { holder: holder.cloned(), who: who.cloned(), why }))
    };
    let d = match fresh(lock, proves) {
        Ok(d) => d,
        Err(_) if fs::symlink_metadata(lock).is_err() => return Err(None),
        Err(_) if lock.is_dir() => return blocked(None, None, "held by an older version that records no holder".into()),
        Err(read) => {
            let read = if read.is_empty() { "empty".to_string() } else { read };
            return blocked(None, None, format!("its record names no holder ({read})"));
        }
    };
    let mut chain = vec![d.clone()];
    loop {
        let x = chain.last().unwrap().clone();
        let (verdict, why) = judge(&x, me);
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
        let marker = PathBuf::from(format!("{}.reclaim.{}", lock.display(), x.replace(':', ".")));
        match take(lock, &marker, &me.id) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => return blocked(Some(&d), Some(&x), format!("its reclaim marker {} cannot be read ({e})", marker.display())),
        }
        match fresh(&marker, proves) {
            Ok(r) => chain.push(r),
            Err(_) => return blocked(Some(&d), Some(&x), format!("its reclaim marker {} cannot be read", marker.display())),
        }
    }
    std::thread::sleep(settle);
    match fresh(lock, proves) {
        Ok(now) if chain.contains(&now) => {
            let _ = fs::remove_file(lock);
            Ok(())
        }
        Ok(now) => blocked(Some(&now), None, "it was taken again during the reclaim".into()),
        Err(_) if fs::symlink_metadata(lock).is_err() => Ok(()),
        Err(_) => blocked(Some(&d), None, "its record changed during the reclaim".into()),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::io::{BufRead, BufReader};

    const LIB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../comm/core/scripts/comm-lib.sh");

    /// Held by every test here: the records read `SOT_SELF_HOST`, which other
    /// tests in this binary set, and the shell children inherit the env.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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

    /// `bash -c` with comm-lib.sh sourced on `home`; the body's first line of
    /// stdout is returned once printed, and the child is left running.
    fn shell(home: &Path, body: &str) -> (Child, String) {
        let mut c = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{LIB}'; {body}"))
            .env("SOT_COMM_HOME", home)
            .env("SOT_COMM_TEST_LOCK_SETTLE", "0.05")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(c.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        (c, line.trim().to_string())
    }

    fn run(home: &Path, body: &str) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{LIB}'; {body}"))
            .env("SOT_COMM_HOME", home)
            .env("SOT_COMM_TEST_LOCK_SETTLE", "0.05")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A shell holder SIGKILLed inside its critical section.
    fn dead_shell_holder(home: &Path) {
        let _ = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{LIB}'; with_lock kill -9 $$"))
            .env("SOT_COMM_HOME", home)
            .status()
            .unwrap();
        assert!(home.join(".registry.lock").is_file(), "the killed holder left its lock");
    }

    #[test]
    fn the_shell_and_rust_records_are_byte_equal_and_judged_alike() {
        let _env = env_lock();
        let home = scratch("parity");
        let (mut child, shell_id) = shell(&home, "_sot_lock_self_id; echo \"$_SOT_LOCK_ID\"; exec sleep 30");
        let pid = child.id();
        assert_eq!(record_for(pid), shell_id, "Rust's record for the shell's pid");
        let me = Me::now();
        assert!(matches!(judge(&shell_id, &me).0, Verdict::Alive));
        let q = format!("_sot_lock_self_id; _sot_lock_judge '{shell_id}'; echo $_SOT_LOCK_VERDICT");
        assert_eq!(run(&home, &q), "ALIVE");
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(judge(&shell_id, &me).0, Verdict::Dead));
        assert_eq!(run(&home, &q), "DEAD");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn rust_reclaims_a_lock_whose_shell_holder_was_killed() {
        let _env = env_lock();
        let home = scratch("reclaim");
        dead_shell_holder(&home);
        let dead = fs::read_to_string(home.join(".registry.lock")).unwrap();
        let lock = home.join(".registry.lock");
        let held = acquire_with(&lock, Duration::from_secs(2), Duration::from_millis(50)).expect("reclaimed");
        let marker = format!("{}.reclaim.{}", lock.display(), dead.trim().replace(':', "."));
        assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{}\n", Me::now().id), "the marker is mine");
        drop(held);
        assert!(!lock.exists(), "released");
        assert!(Path::new(&marker).exists(), "markers are kept");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_live_shell_holder_is_named_and_never_reclaimed() {
        let _env = env_lock();
        let home = scratch("live");
        let (mut child, _) = shell(&home, "with_lock bash -c 'echo held; exec sleep 30'");
        let lock = home.join(".registry.lock");
        let before = fs::read(&lock).unwrap();
        let err = acquire_with(&lock, Duration::from_millis(200), Duration::from_millis(50)).err().expect("FAILED");
        let f: Vec<String> = String::from_utf8_lossy(&before).trim().split(':').map(str::to_string).collect();
        let want = format!("is held by {} pid {} start {} (", f[0], f[4], f[5]);
        assert!(err.contains(&want), "{err}");
        assert!(err.ends_with(&format!(
            "): it is running. If it is dead, run any comm command on {}, or run comm-registry-lock-clear.sh.",
            f[0]
        )), "{err}");
        assert_eq!(fs::read(&lock).unwrap(), before, "the lock is untouched");
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn one_rust_thread_and_two_shell_waiters_hold_one_at_a_time() {
        let _env = env_lock();
        let home = scratch("race");
        let lock = home.join(".registry.lock");
        let cs = home.join("cs");
        let crit = format!("crit() {{ mkdir '{0}' || echo OVERLAP; sleep 0.01; rmdir '{0}'; echo held; }}", cs.display());
        for round in 0..100 {
            dead_shell_holder(&home);
            let waiters: Vec<Child> = (0..2)
                .map(|_| {
                    Command::new("bash")
                        .arg("-c")
                        .arg(format!(". '{LIB}'; {crit}; with_lock crit"))
                        .env("SOT_COMM_HOME", &home)
                        .env("SOT_COMM_TEST_LOCK_SETTLE", "0.05")
                        .stdout(Stdio::piped())
                        .spawn()
                        .unwrap()
                })
                .collect();
            let held = acquire_with(&lock, Duration::from_secs(10), Duration::from_millis(50)).expect("rust took it");
            assert!(fs::create_dir(&cs).is_ok(), "round {round}: overlap with the Rust holder");
            std::thread::sleep(Duration::from_millis(10));
            fs::remove_dir(&cs).unwrap();
            drop(held);
            for w in waiters {
                let out = w.wait_with_output().unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "held", "round {round}");
            }
            assert!(!lock.exists(), "round {round}: released");
            let markers = fs::read_dir(&home).unwrap().filter(|e| {
                e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".registry.lock.reclaim.")
            });
            assert_eq!(markers.count(), round + 1, "round {round}: one new marker");
        }
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
}
