//! A Julia child's own temporary folder, and for a child that runs user code its own display: made before its spawn,
//! named in its environment, ended after its reap; a display that ends first ends the child. Every child folder is made in
//! this process's own folder of them, a held folder (`sot_log::host::HeldDir`), which goes with the last of them, at the
//! daemon's controlled end ([`remove_own`]), or, for a process that ended without either, at a later start
//! ([`sweep_gone`]).

use crate::lifecycle::child_signal::{Contained, Signal};
use sot_log::host::HeldDir;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tokio::process::Command;

/// The variables Julia's `tempdir()` reads on this platform.
#[cfg(unix)]
const TMP_VARS: &[&str] = &["TMPDIR"];
#[cfg(windows)]
const TMP_VARS: &[&str] = &["TMP", "TEMP"];

/// How a process's folder of child folders is named in its temporary folder: a random part follows
/// (`sotd-julia-<random>`).
const PREFIX: &str = "sotd-julia-";

/// This process's folder of child folders and how many it holds: made with the first and removed with the last, or at
/// the daemon's controlled end ([`remove_own`]), and held by its lock until then.
static OWN: Mutex<Option<(HeldDir, usize)>> = Mutex::new(None);

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
    /// The folder's count in this process's held folder; dropped after `dir`.
    held: InHeld,
}

impl ChildTmp {
    pub(crate) fn new() -> std::io::Result<Self> {
        let held = InHeld::enter()?;
        let dir = tempfile::Builder::new().prefix("child-").tempdir_in(&held.0)?;
        Ok(Self {
            #[cfg(target_os = "linux")]
            display: None,
            dir,
            held,
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
                // The held folder stays with it, to the daemon's end or a later start.
                std::mem::forget(self.held);
                tracing::warn!(path = %kept.display(), error = %e, "sot-julia temporary folder kept: its child is not confirmed ended");
                return Err(e);
            }
        };
        let (dir, held) = (self.dir, self.held);
        let path = dir.path().to_path_buf();
        drop(tokio::task::spawn_blocking(move || {
            if let Err(e) = dir.close() {
                tracing::warn!(path = %path.display(), error = %e, "sot-julia temporary folder not removed");
            }
            drop(held);
        }));
        Ok(status)
    }
}

/// One child folder's count in this process's held folder, the one at its path. Dropped once its child folder is gone,
/// it removes the held folder when it was the last.
struct InHeld(PathBuf);

impl InHeld {
    /// Counts one more child folder in this process's held folder, made if there is none.
    fn enter() -> std::io::Result<Self> {
        let mut own = OWN.lock().unwrap_or_else(PoisonError::into_inner);
        let (held, count) = match own.take() {
            Some(own) => own,
            None => (HeldDir::create(&std::env::temp_dir(), PREFIX).map_err(std::io::Error::other)?, 0),
        };
        let path = held.path().to_path_buf();
        *own = Some((held, count + 1));
        Ok(Self(path))
    }
}

impl Drop for InHeld {
    fn drop(&mut self) {
        let mut own = OWN.lock().unwrap_or_else(PoisonError::into_inner);
        let last = match own.as_mut() {
            Some((held, count)) if held.path() == self.0 => {
                *count -= 1;
                *count == 0
            }
            // `remove_own` took it, or this count is an earlier held folder's.
            _ => false,
        };
        let last = if last { own.take() } else { None };
        drop(own);
        if let Some((held, _)) = last {
            if let Err(e) = held.remove() {
                tracing::warn!(path = %self.0.display(), error = %e, "sot-julia temporary folders not removed; the next start removes them");
            }
        }
    }
}

/// Removes this process's folder of child folders, at the daemon's controlled end (`lifecycle::shutdown::exit`), after
/// the fire has asked every child to end, though its children's owners hold their folders still. The removal runs on a
/// thread of its own, waited for at most `bound`, so a large folder does not hold the exit; a folder not removed by then,
/// or that a child in its last moment wrote into, is the next start's ([`sweep_gone`]), its lock freed by this process's
/// end. A process that holds no child folder removes nothing. With no thread to run it on, nothing is removed.
pub(crate) fn remove_own(bound: Duration) {
    let (sent, removed) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("sotd-child-folders".into())
        .spawn(move || {
            let own = OWN.lock().unwrap_or_else(PoisonError::into_inner).take();
            let _ = sent.send(own.map(|(held, _)| held.remove()));
        });
    if started.is_err() {
        return;
    }
    match removed.recv_timeout(bound) {
        Ok(Some(Err(e))) => tracing::warn!(error = %e, "sot-julia temporary folders not all removed; the next start removes the rest"),
        Err(_) => tracing::warn!("sot-julia temporary folders not all removed within {bound:?}; the next start removes the rest"),
        Ok(_) => {}
    }
}

/// Removes the folders of child folders of every process of this account that has ended (each folder's lock is free), on
/// a blocking thread nothing waits for: the daemon's start (`server::run`), for the ends that run no code in the daemon
/// (a crash, a SIGKILL of the daemon or of its guard). A running process's folder stays, this one's included, whatever
/// computer or pid namespace it runs in, and so does a folder named as before held folders (`sot-julia-*`).
pub(crate) fn sweep_gone() {
    drop(tokio::task::spawn_blocking(|| {
        let removed = sot_log::host::remove_ended(&std::env::temp_dir(), PREFIX);
        if removed > 0 {
            tracing::info!(removed, "sot-julia temporary folders of ended daemons removed");
        }
    }));
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

    /// Every child folder is made in this process's held folder of them, `sotd-julia-<random>` in its temporary folder,
    /// which is made with the first and goes with the last. Run in a process of its own with a temporary folder of its
    /// own, so no other test's child folder counts.
    #[test]
    fn the_held_folder_comes_with_the_first_child_folder_and_goes_with_the_last() {
        if !crate::sidecars::contract_tests::isolated(
            "sidecars::child::tmp::tests::the_held_folder_comes_with_the_first_child_folder_and_goes_with_the_last",
            Duration::from_secs(30),
        ) {
            return;
        }
        let owned = tempfile::tempdir().expect("owned temporary folder");
        for key in super::TMP_VARS {
            std::env::set_var(key, owned.path());
        }
        let first = ChildTmp::new().expect("a child folder");
        let second = ChildTmp::new().expect("a second child folder");
        let held = first.dir.path().parent().expect("its folder").to_path_buf();
        assert_eq!(held.parent(), Some(owned.path()), "{held:?}");
        let name = held.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        assert!(name.starts_with(super::PREFIX), "{held:?}");
        assert_eq!(second.dir.path().parent(), Some(held.as_path()), "one held folder for all of this process's child folders");
        drop(first);
        assert!(held.is_dir(), "the held folder stays while a child folder is in it");
        drop(second);
        let left: Vec<_> = std::fs::read_dir(owned.path()).expect("owned folder").flatten().map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "the held folder goes with the last child folder: {left:?}");
    }
}
