//! A Julia child's own temporary folder: made before its spawn, named in its environment, removed after its reap.

use crate::lifecycle::child_signal::Contained;
use tokio::process::Command;

/// The variables Julia's `tempdir()` reads on this platform.
#[cfg(unix)]
const TMP_VARS: &[&str] = &["TMPDIR"];
#[cfg(windows)]
const TMP_VARS: &[&str] = &["TMP", "TEMP"];

/// A folder of its own, in this process's temporary folder, for one Julia child and everything it starts. Julia
/// removes its temporary files only at a clean exit, never when the daemon kills it, so the folder holds what a killed
/// child leaves. An owner ends its child with [`retire`](Self::retire), which removes the folder after the reap.
/// Dropping a `ChildTmp` removes the folder at once, so it is only for an owner whose child never started: a folder
/// dropped after a spawn may still hold a live child's files.
pub(crate) struct ChildTmp(tempfile::TempDir);

impl ChildTmp {
    pub(crate) fn new() -> std::io::Result<Self> {
        tempfile::Builder::new()
            .prefix("sot-julia-")
            .tempdir()
            .map(Self)
    }

    /// Names the folder as `cmd`'s temporary folder.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        for key in TMP_VARS {
            cmd.env(key, self.0.path());
        }
    }

    /// Ends `child` (its tree, then its direct child's reap) and removes the folder. The removal walks the folder on a
    /// blocking thread that nothing waits for, so a large folder holds neither the caller nor the restart it serves.
    /// A failed kill may leave the child running, so then the folder stays, and its path is logged.
    pub(crate) async fn retire(self, child: &mut Contained) -> std::io::Result<std::process::ExitStatus> {
        let status = match child.kill().await {
            Ok(status) => status,
            Err(e) => {
                let kept = self.0.keep();
                tracing::warn!(path = %kept.display(), error = %e, "sot-julia temporary folder kept: its child is not confirmed ended");
                return Err(e);
            }
        };
        let path = self.0.path().to_path_buf();
        drop(tokio::task::spawn_blocking(move || {
            if let Err(e) = self.0.close() {
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

    /// A folder with a file in it is removed once its child is ended, and not before the reap.
    #[tokio::test]
    async fn retire_removes_the_folder_after_the_reap() {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let tmp = ChildTmp::new().expect("temporary folder");
        let folder = tmp.0.path().to_path_buf();
        std::fs::write(folder.join("made"), b"x").expect("setup: a file in the folder");
        let mut cmd = long_child();
        let mut child = sig.spawn(&mut cmd).expect("spawn the child");
        let status = tmp.retire(&mut child).await.expect("retire the child");
        assert!(!status.success(), "the child was ended, not left to exit");
        within(Duration::from_secs(30), "the folder is removed after the reap", || !folder.exists()).await;
    }
}
