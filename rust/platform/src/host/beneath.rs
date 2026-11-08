//! A write beneath a root that follows no link: the root is opened once, each folder below it is opened relative to the
//! one before, and the file is created, renamed and synced relative to the last folder. No path is used after the root.

use std::ffi::{CStr, CString};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use libc::c_int;

/// The points a test may stop at, each after the step it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Root,
    Folder(usize),
    Temp,
    Written,
    Renamed,
}

const DIR_FLAGS: c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Writes `bytes` at `rel` beneath `root`, creating missing folders, durably, following no link. `root` is an absolute
/// canonical folder and `rel` names joined by `/`; the result is `root` joined with `rel`, for replies and not for IO.
/// A name or folder the rule refuses is `InvalidInput`.
pub fn write_beneath(root: &Path, rel: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    write_beneath_at(root, rel, bytes, None, |_| Ok(()))
}

/// [`write_beneath`] with a fixed temporary name and `at` run after each step (a test seam).
pub(crate) fn write_beneath_at(
    root: &Path,
    rel: &str,
    bytes: &[u8],
    temp: Option<&str>,
    mut at: impl FnMut(Step) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let names = plain_names(rel)?;
    let (last, folders) = names.split_last().expect("plain_names returns at least one name");
    let croot = CString::new(root.as_os_str().as_bytes()).map_err(|_| invalid("root", "holds a NUL"))?;
    let raw = unsafe { libc::open(croot.as_ptr(), DIR_FLAGS) };
    let mut dir = own(raw).map_err(|e| folder_error(e, "the root"))?;
    at(Step::Root)?;
    for (i, name) in folders.iter().enumerate() {
        dir = open_folder(&dir, name)?;
        at(Step::Folder(i))?;
    }
    let temp = match temp {
        Some(t) => t.to_owned(),
        None => {
            let mut r = [0u8; 8];
            getrandom::fill(&mut r)?;
            format!(".sot-{}.tmp", r.iter().map(|b| format!("{b:02x}")).collect::<String>())
        }
    };
    let ctemp = cstr(&temp)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = own(unsafe { libc::openat(dir.as_raw_fd(), ctemp.as_ptr(), flags, 0o666 as c_int) })?;
    let result = publish(&dir, std::fs::File::from(fd), &ctemp, &cstr(last)?, bytes, &mut at);
    if result.is_err() {
        // The temporary name is ours alone; after a rename it is already gone, and a failure here changes nothing.
        unsafe { libc::unlinkat(dir.as_raw_fd(), ctemp.as_ptr(), 0) };
    }
    result?;
    Ok(root.join(rel))
}

/// Steps 4 to 7 after the create: write, sync, rename within the folder, check the entry, sync the folder.
fn publish(
    dir: &OwnedFd,
    mut file: std::fs::File,
    temp: &CStr,
    last: &CStr,
    bytes: &[u8],
    at: &mut impl FnMut(Step) -> io::Result<()>,
) -> io::Result<()> {
    at(Step::Temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    at(Step::Written)?;
    let meta = file.metadata()?;
    check(unsafe { libc::renameat(dir.as_raw_fd(), temp.as_ptr(), dir.as_raw_fd(), last.as_ptr()) })?;
    at(Step::Renamed)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstatat(dir.as_raw_fd(), last.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    if st.st_dev as u64 != meta.dev() || st.st_ino as u64 != meta.ino() {
        return Err(io::Error::other("the written file is not the entry at its name"));
    }
    check(unsafe { libc::fsync(dir.as_raw_fd()) })?;
    Ok(())
}

/// Step 1: names joined by `/`, each non-empty, not `.` or `..`, none holding a NUL.
fn plain_names(rel: &str) -> io::Result<Vec<&str>> {
    let names: Vec<&str> = rel.split('/').collect();
    match names.iter().find(|n| n.is_empty() || **n == "." || **n == ".." || n.contains('\0')) {
        Some(bad) => Err(invalid(bad, "is not one plain name")),
        None => Ok(names),
    }
}

/// Step 3 for one name: open it below `prev`, creating it when missing; a link or a file at the name is refused.
fn open_folder(prev: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    let cname = cstr(name)?;
    let open = || own(unsafe { libc::openat(prev.as_raw_fd(), cname.as_ptr(), DIR_FLAGS) });
    match open() {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            // An existing entry (EEXIST) is not success: the open below decides what it is.
            match check(unsafe { libc::mkdirat(prev.as_raw_fd(), cname.as_ptr(), 0o777) }) {
                Err(e) if e.raw_os_error() != Some(libc::EEXIST) => return Err(e),
                _ => {}
            }
            let fd = open().map_err(|e| folder_error(e, name))?;
            check(unsafe { libc::fsync(prev.as_raw_fd()) })?;
            Ok(fd)
        }
        r => r.map_err(|e| folder_error(e, name)),
    }
}

/// A link (ELOOP, or ENOTDIR under the no-follow directory open) or a file at a folder's name.
fn folder_error(e: io::Error, name: &str) -> io::Error {
    match e.raw_os_error() {
        Some(libc::ENOTDIR | libc::ELOOP) => invalid(name, "is not a plain folder"),
        _ => e,
    }
}

fn invalid(name: &str, what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} {what}"))
}

fn cstr(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| invalid(s, "holds a NUL"))
}

fn check(r: c_int) -> io::Result<()> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn own(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
}

#[cfg(test)]
#[path = "beneath_tests.rs"]
mod tests;
