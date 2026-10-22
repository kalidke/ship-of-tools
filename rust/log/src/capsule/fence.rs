//! The leg's folder, `<state_dir>/voyages/<id>/`: its writer fence (`writer.lock`) and the small file the leg leaves
//! when it ends (`ended`). No record of the terminal is kept in it.
use super::*;
use sot_platform::host::WriterLock;

/// The writer fence's lock file, persistent and never unlinked.
pub const WRITER_LOCK: &str = "writer.lock";

/// Creates the folder, protected from birth, with its fence file; a folder that is already there is left as it is.
pub fn bootstrap(root: &std::path::Path) -> Result<()> {
    match sot_platform::host::create_dir_protected(root) {
        Ok(()) => {}
        Err(_) if root.is_dir() => {}
        Err(e) => return Err(e.into()),
    }
    let lock = root.join(WRITER_LOCK);
    if !lock.exists() {
        std::fs::File::create(&lock)?.sync_all()?;
    }
    sot_platform::host::fsync_dir(root)?;
    if let Some(parent) = root.parent() {
        sot_platform::host::fsync_dir(parent)?;
    }
    Ok(())
}

/// Takes the writer fence of the folder at `root` and checks that its volume can take a write.
pub(super) fn take_fence(root: &std::path::Path) -> Result<WriterLock> {
    let root = std::fs::canonicalize(root)?;
    let lock = sot_platform::host::lock_writer(&root.join(WRITER_LOCK))?;
    sot_platform::host::preflight_volume(&root)?;
    Ok(lock)
}

/// The file holding the number of legs that have started in the folder.
const LEG_FILE: &str = "leg";

/// Counts a new leg: reads the folder's leg number, writes the next durably and returns it (1 for the first leg).
pub(super) fn next_leg_epoch(root: &std::path::Path) -> Result<u64> {
    let path = root.join(LEG_FILE);
    let before = read_leg_epoch(root).unwrap_or(0);
    let epoch = before + 1;
    sot_platform::host::write_durable(&path, epoch.to_string().as_bytes())?;
    Ok(epoch)
}

/// The number of the leg that last started in the folder at `root`, `None` when none has.
pub fn read_leg_epoch(root: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(root.join(LEG_FILE)).ok()?.trim().parse().ok()
}
