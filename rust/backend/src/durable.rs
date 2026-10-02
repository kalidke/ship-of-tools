// durable.rs — the one durable write and delete for daemon records a later
// start acts on: `held.json` (lease.rs) and a row's `row-scopes`
// (capsule_workspace.rs). A write is a tmp file, fsync, then the replace;
// a delete is durable once its directory is synced.

use std::path::Path;

/// Writes `bytes` to `path` (tmp file beside it, fsync, rename, directory
/// sync).
pub(crate) fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    replace_file(&tmp, path)
}

/// Deletes `path` if it exists, then syncs its directory.
pub(crate) fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        r => r?,
    }
    sync_dir(path)
}

/// The replace step. Unix renames, then syncs the directory so the rename
/// is durable. Windows replaces with MoveFileExW write-through, which
/// returns once the move is on disk.
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let wide = |p: &Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (from, to) = (wide(tmp), wide(path));
        // SAFETY: both buffers are NUL-terminated and outlive the call.
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, path)?;
        sync_dir(path)
    }
}

/// A delete is durable only once its directory is synced. The Windows
/// delete has no sync, since Win32 has no write-through delete: the one
/// Windows residual.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
