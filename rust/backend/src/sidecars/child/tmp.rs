//! A Julia child's own temporary folder, and for a child that runs user code its own display: made before its spawn,
//! named in its environment, ended after its reap; a display that ends first ends the child. The folder's name holds the
//! daemon's pid, so the daemon's controlled end removes its own folders ([`remove_own`]) and a later start removes those
//! of a daemon that ended without one ([`sweep_gone`]).

use crate::lifecycle::child_signal::{Contained, Signal};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// The variables Julia's `tempdir()` reads on this platform.
#[cfg(unix)]
const TMP_VARS: &[&str] = &["TMPDIR"];
#[cfg(windows)]
const TMP_VARS: &[&str] = &["TMP", "TEMP"];

/// How every child folder's name starts: the daemon's pid, a dash and a random part follow (`sot-julia-<pid>-<random>`).
const PREFIX: &str = "sot-julia-";

/// A folder of its own, in this process's temporary folder, for one Julia child and everything it starts. Julia
/// removes its temporary files only at a clean exit, never when the daemon kills it, so the folder holds what a killed
/// child leaves. An owner ends its child with [`retire`](Self::retire), which removes the folder after the reap.
/// Dropping a `ChildTmp` removes the folder at once, so it is for an owner whose child never started (and the pipe-take
/// returns after a spawn, unreachable while every pipe is `Stdio::piped()`): a folder dropped after a spawn may still
/// hold a live child's files.
pub(crate) struct ChildTmp {
    /// The display of a child that runs user code (Linux; `for_user_code`), whose cookie is a file in `dir`. Ended
    /// after the child and before the folder; dropped, its server's group is ended with it.
    #[cfg(target_os = "linux")]
    display: Option<crate::sidecars::display::Display>,
    dir: tempfile::TempDir,
}

impl ChildTmp {
    pub(crate) fn new() -> std::io::Result<Self> {
        let prefix = format!("{PREFIX}{}-", std::process::id());
        let dir = tempfile::Builder::new().prefix(&prefix).tempdir()?;
        Ok(Self {
            #[cfg(target_os = "linux")]
            display: None,
            dir,
        })
    }

    /// As [`new`](Self::new), for a child that runs user code (the REPL, Pluto): on Linux it also starts the child's
    /// own display under `sig` ([`Display`](crate::sidecars::display::Display)). On macOS and Windows GLMakie draws
    /// through the system's own windowing, and nothing is started.
    pub(crate) async fn for_user_code(sig: &'static Signal) -> std::io::Result<Self> {
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut tmp = Self::new()?;
        #[cfg(target_os = "linux")]
        {
            tmp.display = Some(crate::sidecars::display::Display::start(tmp.dir.path(), sig).await);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = sig;
        Ok(tmp)
    }

    /// Names the folder as `cmd`'s temporary folder, and the display, if the child has one, as its display.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        for key in TMP_VARS {
            cmd.env(key, self.dir.path());
        }
        #[cfg(target_os = "linux")]
        if let Some(display) = &self.display {
            display.apply(cmd);
        }
    }

    /// Returns once the child's display has ended under it ([`Display::ended`](crate::sidecars::display::Display::ended));
    /// never for a child with no display. Its owner then ends the child with [`retire`](Self::retire), naming that end.
    pub(crate) async fn display_ended(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(display) = self.display.as_mut() {
            return display.ended().await;
        }
        std::future::pending().await
    }

    /// Ends `child` (its tree, then its direct child's reap), then its display, and removes the folder. The removal
    /// walks the folder on a blocking thread that nothing waits for, so a large folder holds neither the caller nor the
    /// restart it serves. A failed kill may leave the child running, so then the folder stays, and its path is logged;
    /// its display ends all the same.
    pub(crate) async fn retire(self, child: &mut Contained) -> std::io::Result<std::process::ExitStatus> {
        let killed = child.kill().await;
        #[cfg(target_os = "linux")]
        if let Some(display) = self.display {
            display.end().await;
        }
        let status = match killed {
            Ok(status) => status,
            Err(e) => {
                let kept = self.dir.keep();
                tracing::warn!(path = %kept.display(), error = %e, "sot-julia temporary folder kept: its child is not confirmed ended");
                return Err(e);
            }
        };
        let dir = self.dir;
        let path = dir.path().to_path_buf();
        drop(tokio::task::spawn_blocking(move || {
            if let Err(e) = dir.close() {
                tracing::warn!(path = %path.display(), error = %e, "sot-julia temporary folder not removed");
            }
        }));
        Ok(status)
    }
}

/// Removes this process's own child folders, at the daemon's controlled end (`lifecycle::shutdown::exit`), after the
/// fire has asked every child to end: the folders of the children it still held, and any an earlier process of the same
/// pid left. The removal runs on a thread of its own, waited for at most `bound`, so a large folder does not hold the
/// exit; a folder not removed by then, or that a child in its last moment wrote into, is the next start's
/// ([`sweep_gone`]). With no thread to run it on, nothing is removed.
pub(crate) fn remove_own(bound: Duration) {
    let own = std::process::id();
    let (sent, removed) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("sotd-child-folders".into())
        .spawn(move || {
            let _ = sent.send(remove_where(&std::env::temp_dir(), |pid| pid == own));
        });
    if started.is_ok() && removed.recv_timeout(bound).is_err() {
        tracing::warn!("sot-julia temporary folders not all removed within {bound:?}; the next start removes the rest");
    }
}

/// Removes the child folders of every daemon of this account whose pid is not running, on a blocking thread nothing
/// waits for: the daemon's start (`server::run`), for the ends that run no code in the daemon (a crash, a SIGKILL of the
/// daemon or of its guard). A running daemon's folders stay, this one's included, and so does a folder whose name holds
/// no pid (`sot-julia-<random>`, a daemon's from before the pid was in the name), whose owner nothing names.
pub(crate) fn sweep_gone() {
    drop(tokio::task::spawn_blocking(|| {
        let removed = remove_where(&std::env::temp_dir(), |pid| !running(pid));
        if removed > 0 {
            tracing::info!(removed, "sot-julia temporary folders of ended daemons removed");
        }
    }));
}

/// Removes each child folder in `dir` whose name holds a pid that `pick` picks, and returns how many went. Only a
/// directory this account owns goes, never a link or a file; a folder that does not go is logged and stays, and one
/// another removal took first is not counted.
fn remove_where(dir: &Path, pick: impl Fn(u32) -> bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !owner(&entry.file_name()).is_some_and(&pick) || !own_folder(&entry) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %entry.path().display(), error = %e, "sot-julia temporary folder not removed"),
        }
    }
    removed
}

/// The pid a child folder's name holds (`sot-julia-<pid>-<random>`), or `None` for any other name. Only a pid in
/// `1..=i32::MAX` counts, so a signal sent to it is a process's, never a group's or every process's.
fn owner(name: &std::ffi::OsStr) -> Option<u32> {
    let (pid, random) = name.to_str()?.strip_prefix(PREFIX)?.split_once('-')?;
    if random.is_empty() || pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().filter(|pid| (1..=i32::MAX as u32).contains(pid))
}

/// Whether `entry` is a directory, not a link, that this account owns. On Windows the temporary folder is the
/// account's own, so a directory is enough.
fn own_folder(entry: &std::fs::DirEntry) -> bool {
    // A `DirEntry`'s metadata is the entry's own: a link is not followed.
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.is_dir() && meta.uid() == crate::paths::current_uid()
    }
    #[cfg(not(unix))]
    meta.is_dir()
}

/// Whether the process `pid` may be running. One this account may not signal or open counts as running, and so does a
/// zombie, so a folder goes only when its owner is certainly gone.
#[cfg(unix)]
fn running(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; `owner` passes only pids in 1..=i32::MAX, never 0 or -1 (a group, every process).
    let probed = unsafe { libc::kill(pid as libc::pid_t, 0) };
    probed == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(windows)]
fn running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    // SAFETY: a handle opened, polled with no wait and closed here.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        running
    }
}

#[cfg(test)]
mod tests {
    use super::ChildTmp;
    use crate::lifecycle::child_signal::Signal;
    use crate::sidecars::contract_tests::{executable, within};
    use std::time::Duration;
    use tokio::process::Command;

    /// A child that outlives the test unless it is ended.
    fn long_child() -> Command {
        #[cfg(unix)]
        {
            let mut cmd = Command::new(executable("sleep"));
            cmd.arg("30");
            cmd
        }
        #[cfg(windows)]
        {
            let mut cmd = Command::new(executable("powershell"));
            cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
            cmd
        }
    }

    /// A folder with a file in it is removed once its child is ended. The order (after the reap) is `retire`'s own; this
    /// test sees only that the folder goes.
    #[tokio::test]
    async fn retire_removes_the_folder_after_the_reap() {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let tmp = ChildTmp::new().expect("temporary folder");
        let folder = tmp.dir.path().to_path_buf();
        std::fs::write(folder.join("made"), b"x").expect("setup: a file in the folder");
        let mut cmd = long_child();
        let mut child = sig.spawn(&mut cmd).expect("spawn the child");
        let status = tmp.retire(&mut child).await.expect("retire the child");
        assert!(!status.success(), "the child was ended, not left to exit: {status}");
        within(Duration::from_secs(30), "the folder is removed after the reap", || !folder.exists()).await;
    }

    /// A child folder's name holds the pid of the process that made it.
    #[test]
    fn a_child_folder_names_its_daemon() {
        let tmp = ChildTmp::new().expect("temporary folder");
        let name = tmp.dir.path().file_name().expect("a name").to_owned();
        assert_eq!(super::owner(&name), Some(std::process::id()), "{name:?}");
    }

    /// The rule of both removals, as a table over a folder of the test's own: a child folder goes when its name holds a
    /// pid the caller picks (the end: its own; the start: one not running), with everything in it, and nothing else
    /// goes: not another pid's folder, not a name with no pid (an older daemon's), a malformed pid or pid 0, not a file
    /// or a link.
    #[test]
    fn a_child_folder_goes_only_when_its_named_owner_is_picked() {
        let root = tempfile::tempdir().expect("a folder of the test's own");
        let own = std::process::id();
        // Above every platform's largest pid, so never running.
        let gone = i32::MAX as u32;
        assert!(!super::running(gone), "premise: pid {gone} is not running");
        assert!(super::running(own), "premise: this process is running");
        let folder = |name: String| {
            let inner = root.path().join(&name).join("inner");
            std::fs::create_dir_all(&inner).expect("setup: a child folder");
            std::fs::write(inner.join("made"), b"x").expect("setup: a file in it");
            name
        };
        let own_child = folder(format!("sot-julia-{own}-Ab12Cd"));
        let gone_child = folder(format!("sot-julia-{gone}-Ef34Gh"));
        let mut kept = vec![
            folder("sot-julia-Ij56Kl".to_string()),
            folder(format!("sot-julia-{gone}-")),
            folder(format!("sot-julia-+{gone}-Mn78Op")),
            folder("sot-julia-0-Qr90St".to_string()),
            folder(format!("sot-other-{gone}-Uv12Wx")),
        ];
        let file = format!("sot-julia-{gone}-file");
        std::fs::write(root.path().join(&file), b"x").expect("setup: a file named as a child folder");
        kept.push(file);
        #[cfg(unix)]
        {
            let link = format!("sot-julia-{gone}-link");
            std::os::unix::fs::symlink(root.path().join(&kept[0]), root.path().join(&link)).expect("setup: a link");
            kept.push(link);
        }
        let present = |name: &str| root.path().join(name).symlink_metadata().is_ok();

        assert_eq!(super::remove_where(root.path(), |pid| pid == own), 1, "the end removes its own folder alone");
        assert!(!present(&own_child), "{own_child} is removed");
        assert_eq!(super::remove_where(root.path(), |pid| !super::running(pid)), 1, "the start removes the gone pid's alone");
        assert!(!present(&gone_child), "{gone_child} is removed");
        for name in &kept {
            assert!(present(name), "{name} stays");
        }
        assert!(root.path().join(&kept[0]).join("inner").join("made").exists(), "a kept folder keeps its files, a link's target included");
    }
}
