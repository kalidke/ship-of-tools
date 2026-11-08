//! The Unix arm of the write and the new folder beneath a root (`write_beneath_at`, `create_dir_beneath_at`): `openat`
//! relative to the folder before, no link followed; and of a record's write in its own folder (`open_followed`,
//! `remove_in`, `write_in`), the folder opened once by its path, a link followed, and every step relative to it.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use libc::c_int;

use super::{absolute_root, invalid, plain_names, temp_name, Step};

const DIR_FLAGS: c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// The Unix [`super::write_beneath_at`].
pub(super) fn write_beneath_at(
    root: &Path,
    rel: &str,
    bytes: &[u8],
    temp: Option<&str>,
    mut at: impl FnMut(Step) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let mut names = names_of(rel)?;
    let last = names.pop().expect("plain_names returns at least one name");
    let ctemp = CString::new(temp_name(temp)?).map_err(|_| invalid("the temporary name holds a NUL".into()))?;
    let dir = walk(root, &names, true, &mut at)?;
    create_and_publish(&dir, &ctemp, &last, bytes, &mut at)?;
    Ok(root.join(rel))
}

/// Opens `folder` by its path, following links: the folder of a record the daemon writes in its own place. Only a
/// folder opens, so a pipe or device at its name answers at once.
pub(super) fn open_followed(folder: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY).open(folder)
}

/// Removes `name` in `dir`, a link at it as itself; a folder there is an error.
pub(super) fn remove_in(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = CString::new(name.as_bytes()).map_err(|_| invalid("the name holds a NUL".into()))?;
    check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) })
}

/// The names of [`create_and_publish`] from a record's folder.
pub(super) fn write_in(dir: &File, temp: &OsStr, last: &OsStr, bytes: &[u8], at: &mut impl FnMut(Step) -> io::Result<()>) -> io::Result<()> {
    let name = |n: &OsStr| CString::new(n.as_bytes()).map_err(|_| invalid("the name holds a NUL".into()));
    create_and_publish(dir, &name(temp)?, &name(last)?, bytes, at)
}

/// Create the temporary file in `dir`, then write, rename and sync.
fn create_and_publish(dir: &File, temp: &CStr, last: &CStr, bytes: &[u8], at: &mut impl FnMut(Step) -> io::Result<()>) -> io::Result<()> {
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = own(unsafe { libc::openat(dir.as_raw_fd(), temp.as_ptr(), flags, 0o666 as c_int) })?;
    let result = publish(dir, File::from(fd), temp, last, bytes, at);
    if result.is_err() {
        // This write created the temporary name exclusively; after a rename it is already gone, and a failure here
        // changes nothing.
        unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0) };
    }
    result
}

/// The Unix [`super::create_dir_beneath_at`]: the walk makes no folder, and the last name is made relative to the
/// parent's handle.
pub(super) fn create_dir_beneath_at(root: &Path, rel: &str, mut at: impl FnMut(Step) -> io::Result<()>) -> io::Result<PathBuf> {
    let mut names = names_of(rel)?;
    let last = names.pop().expect("plain_names returns at least one name");
    let dir = walk(root, &names, false, &mut at)?;
    check(unsafe { libc::mkdirat(dir.as_raw_fd(), last.as_ptr(), 0o777) })?;
    dir.sync_all()?;
    Ok(root.join(rel))
}

/// Every name is refused and converted before the root is opened.
fn names_of(rel: &str) -> io::Result<Vec<CString>> {
    plain_names(rel)?
        .into_iter()
        .map(|n| CString::new(n).map_err(|_| invalid(format!("{n:?} is not one plain name"))))
        .collect()
}

/// Opens the root, then each of `folders` below it, creating the missing ones when `make`; returns the last.
fn walk(root: &Path, folders: &[CString], make: bool, at: &mut impl FnMut(Step) -> io::Result<()>) -> io::Result<File> {
    absolute_root(root)?;
    let croot = CString::new(root.as_os_str().as_bytes()).map_err(|_| invalid(format!("{root:?} holds a NUL")))?;
    // A root that is its own canonical path has no link on its way.
    if std::fs::canonicalize(root).map_err(|e| folder_error(e, "the root"))?.as_os_str().as_bytes() != root.as_os_str().as_bytes() {
        return Err(invalid(format!("{root:?} is not its own canonical path")));
    }
    let root_fd = own(unsafe { libc::open(croot.as_ptr(), DIR_FLAGS) }).map_err(|e| folder_error(e, "the root"))?;
    let mut dir = File::from(root_fd);
    at(Step::Root)?;
    for (i, name) in folders.iter().enumerate() {
        dir = open_folder(&dir, name, make, &mut || at(Step::Missing(i)))?;
        at(Step::Folder(i))?;
    }
    Ok(dir)
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

/// Opens `name` below `prev`, creating it when missing and `make`; a link or a file at the name is refused.
fn open_folder(prev: &File, name: &CStr, make: bool, missing: &mut impl FnMut() -> io::Result<()>) -> io::Result<File> {
    let open = || own(unsafe { libc::openat(prev.as_raw_fd(), name.as_ptr(), DIR_FLAGS) });
    let shown = name.to_string_lossy();
    match open() {
        Err(e) if make && e.raw_os_error() == Some(libc::ENOENT) => {
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
