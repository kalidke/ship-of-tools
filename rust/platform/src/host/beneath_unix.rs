//! The Unix arm of the write beneath a root: `openat` relative to the folder before, no link followed.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use libc::c_int;

use super::{invalid, plain_names, temp_name, Step};

const DIR_FLAGS: c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// The Unix [`super::write_beneath_at`].
pub(super) fn write_beneath_at(
    root: &Path,
    rel: &str,
    bytes: &[u8],
    temp: Option<&str>,
    mut at: impl FnMut(Step) -> io::Result<()>,
) -> io::Result<PathBuf> {
    // Every name is refused and converted before the root is opened.
    let mut names = plain_names(rel)?
        .into_iter()
        .map(|n| CString::new(n).map_err(|_| invalid(format!("{n:?} is not one plain name"))))
        .collect::<io::Result<Vec<_>>>()?;
    let last = names.pop().expect("plain_names returns at least one name");
    let croot = CString::new(root.as_os_str().as_bytes()).map_err(|_| invalid(format!("{root:?} holds a NUL")))?;
    // A root that is its own canonical path has no link on its way.
    if !root.is_absolute() || std::fs::canonicalize(root).map_err(|e| folder_error(e, "the root"))?.as_os_str().as_bytes() != root.as_os_str().as_bytes() {
        return Err(invalid(format!("{root:?} is not its own canonical path")));
    }
    let ctemp = CString::new(temp_name(temp)?).map_err(|_| invalid("the temporary name holds a NUL".into()))?;
    // Open the root, then each folder below it.
    let root_fd = own(unsafe { libc::open(croot.as_ptr(), DIR_FLAGS) }).map_err(|e| folder_error(e, "the root"))?;
    let mut dir = File::from(root_fd);
    at(Step::Root)?;
    for (i, name) in names.iter().enumerate() {
        dir = open_folder(&dir, name, &mut || at(Step::Missing(i)))?;
        at(Step::Folder(i))?;
    }
    // Create the temporary file, then write, rename and sync.
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = own(unsafe { libc::openat(dir.as_raw_fd(), ctemp.as_ptr(), flags, 0o666 as c_int) })?;
    let result = publish(&dir, File::from(fd), &ctemp, &last, bytes, &mut at);
    if result.is_err() {
        // The temporary name is ours alone; after a rename it is already gone, and a failure here changes nothing.
        unsafe { libc::unlinkat(dir.as_raw_fd(), ctemp.as_ptr(), 0) };
    }
    result?;
    Ok(root.join(rel))
}

/// Write, sync, rename within the folder, sync the folder.
fn publish(
    dir: &File,
    mut file: File,
    temp: &CStr,
    last: &CStr,
    bytes: &[u8],
    at: &mut impl FnMut(Step) -> io::Result<()>,
) -> io::Result<()> {
    at(Step::Temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    at(Step::Written)?;
    check(unsafe { libc::renameat(dir.as_raw_fd(), temp.as_ptr(), dir.as_raw_fd(), last.as_ptr()) })?;
    at(Step::Renamed)?;
    dir.sync_all()
}

/// Opens `name` below `prev`, creating it when missing; a link or a file at the name is refused.
fn open_folder(prev: &File, name: &CStr, missing: &mut impl FnMut() -> io::Result<()>) -> io::Result<File> {
    let open = || own(unsafe { libc::openat(prev.as_raw_fd(), name.as_ptr(), DIR_FLAGS) });
    let shown = name.to_string_lossy();
    match open() {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            missing()?;
            // An existing entry (EEXIST) is not success: the open below decides what it is.
            match check(unsafe { libc::mkdirat(prev.as_raw_fd(), name.as_ptr(), 0o777) }) {
                Err(e) if e.raw_os_error() != Some(libc::EEXIST) => return Err(e),
                _ => {}
            }
            let fd = open().map_err(|e| folder_error(e, &format!("{shown:?}")))?;
            prev.sync_all()?;
            Ok(File::from(fd))
        }
        r => r.map(File::from).map_err(|e| folder_error(e, &format!("{shown:?}"))),
    }
}

/// A link (ELOOP, or ENOTDIR under the no-follow directory open) or a file at a folder's name.
fn folder_error(e: io::Error, what: &str) -> io::Error {
    match e.raw_os_error() {
        Some(libc::ENOTDIR | libc::ELOOP) => invalid(format!("{what} is not a plain folder")),
        _ => e,
    }
}

fn check(r: c_int) -> io::Result<()> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn own(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
}
