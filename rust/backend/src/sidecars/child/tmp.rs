//! A Julia child's own temporary folder, and for a child that runs user code its own display: made before its spawn,
//! named in its environment, ended after its reap.

use crate::lifecycle::child_signal::{Contained, Signal};
use tokio::process::Command;

/// The variables Julia's `tempdir()` reads on this platform.
#[cfg(unix)]
const TMP_VARS: &[&str] = &["TMPDIR"];
#[cfg(windows)]
const TMP_VARS: &[&str] = &["TMP", "TEMP"];

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
        let dir = tempfile::Builder::new().prefix("sot-julia-").tempdir()?;
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
}
