//! A Julia child's own temporary folder: made before its spawn, named in its environment, removed after its reap.

use tokio::process::Command;

/// The variables Julia's `tempdir()` reads on this platform.
#[cfg(unix)]
const TMP_VARS: &[&str] = &["TMPDIR"];
#[cfg(windows)]
const TMP_VARS: &[&str] = &["TMP", "TEMP"];

/// A folder of its own, in this process's temporary folder, for one Julia child and everything it starts. Julia
/// removes its temporary files only at a clean exit, never when the daemon kills it, so the folder holds what a killed
/// child leaves; dropping it, after the child's reap, removes the folder whole.
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
}
