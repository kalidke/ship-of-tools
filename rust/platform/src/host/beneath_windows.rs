//! The Windows arm of the write beneath a root: each name is its own `NtCreateFile` relative to the handle before, a
//! name-surrogate reparse point (symlink, junction, mount point) is refused, and the temporary file is renamed by its
//! handle.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_CREATE, FILE_DIRECTORY_FILE,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, GENERIC_WRITE, HANDLE, NTSTATUS, STATUS_INVALID_PARAMETER, STATUS_NOT_A_DIRECTORY,
    STATUS_OBJECT_NAME_NOT_FOUND, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileDispositionInfo, GetFileInformationByHandleEx, SetFileInformationByHandle, DELETE,
    FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TAG_INFO,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};
use windows_sys::Win32::System::Kernel::OBJ_CASE_INSENSITIVE;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{absolute_root, invalid, names_another_place, plain_names, temp_name, Step};
use crate::host::durable::is_transient_hold;
use crate::host::paths::simplify_verbatim;
use crate::host::{RETRY_DEADLINE_MS, RETRY_STEP_MS};

const FOLDER_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// The Windows [`super::write_beneath_at`].
pub(super) fn write_beneath_at(
    root: &Path,
    rel: &str,
    bytes: &[u8],
    temp: Option<&str>,
    mut at: impl FnMut(Step) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let names = plain_names(rel)?;
    let (last, folders) = names.split_last().expect("plain_names returns at least one name");
    let temp = temp_name(temp)?;
    absolute_root(root)?;
    // A root is its own canonical path when std's canonical form, de-verbatimed, equals it; the drive letter and
    // names compare without ASCII case, as the volume does.
    let canonical = std::fs::canonicalize(root)?;
    let same = |a: PathBuf, b: PathBuf| a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy());
    if !same(simplify_verbatim(canonical), simplify_verbatim(root.to_path_buf())) {
        return Err(invalid(format!("{root:?} is not its own canonical path")));
    }
    let mut dir = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root)?;
    refuse_link(&dir, "the root")?;
    at(Step::Root)?;
    for (i, name) in folders.iter().enumerate() {
        dir = open_folder(&dir, name, &mut || at(Step::Missing(i)))?;
        at(Step::Folder(i))?;
    }
    // Create the temporary file exclusively, held with share-read alone so no other open can rename or delete it.
    let file = nt_open(&dir, &temp, GENERIC_WRITE | DELETE | SYNCHRONIZE, FILE_SHARE_READ, FILE_CREATE, FILE_NON_DIRECTORY_FILE)
        .map_err(status_error)?;
    let mut renamed = false;
    let result = publish(&dir, &file, last, bytes, &mut renamed, &mut at);
    if !renamed {
        // The handle names the temporary file alone; a failure here changes nothing.
        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        unsafe {
            SetFileInformationByHandle(file.as_raw_handle() as HANDLE, FileDispositionInfo, &info as *const _ as *const c_void, size_of::<FILE_DISPOSITION_INFO>() as u32)
        };
    }
    result?;
    Ok(root.join(rel))
}

/// Write, sync, rename within the folder, sync the folder.
fn publish(
    dir: &File,
    mut file: &File,
    last: &str,
    bytes: &[u8],
    renamed: &mut bool,
    at: &mut impl FnMut(Step) -> io::Result<()>,
) -> io::Result<()> {
    at(Step::Temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    at(Step::Written)?;
    rename_into(file, dir, last)?;
    *renamed = true;
    at(Step::Renamed)?;
    dir.sync_all()
}

/// Renames the open `file` to `name` in `dir`, replacing an entry there; retried while a scanner holds the name.
fn rename_into(file: &File, dir: &File, name: &str) -> io::Result<()> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    let head = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    // The call refuses a length below the whole structure, which a one-unit name does not fill.
    let len = (head + wide.len() * 2).max(size_of::<FILE_RENAME_INFO>());
    let mut buf = vec![0u64; len.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    unsafe {
        (*info).Anonymous.Flags = 1; // replace if it exists
        (*info).RootDirectory = dir.as_raw_handle() as HANDLE;
        (*info).FileNameLength = (wide.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), std::ptr::addr_of_mut!((*info).FileName) as *mut u16, wide.len());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let s = unsafe { NtSetInformationFile(file.as_raw_handle() as HANDLE, &mut iosb, info as *const c_void, len as u32, FileRenameInformation) };
        if s >= 0 {
            return Ok(());
        }
        let e = status_error(s);
        if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
            return Err(e);
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
}

/// Opens `name` below `prev`, creating it when missing; a link or a file at the name is refused.
fn open_folder(prev: &File, name: &str, missing: &mut impl FnMut() -> io::Result<()>) -> io::Result<File> {
    let open = |disposition| nt_open(prev, name, FOLDER_ACCESS, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, disposition, FILE_DIRECTORY_FILE);
    let (dir, made) = match open(FILE_OPEN) {
        // Open-or-create: an entry put at the name meanwhile is opened as itself and judged below.
        Err(s) if s == STATUS_OBJECT_NAME_NOT_FOUND => {
            missing()?;
            (open(FILE_OPEN_IF), true)
        }
        r => (r, false),
    };
    let dir = dir.map_err(|s| if s == STATUS_NOT_A_DIRECTORY { invalid(format!("{name:?} is not a plain folder")) } else { status_error(s) })?;
    refuse_link(&dir, name)?;
    if made {
        prev.sync_all()?;
    }
    Ok(dir)
}

/// One name relative to `parent`, never following a reparse point at the name; the handle is synchronous so std may use it.
fn nt_open(parent: &File, name: &str, access: u32, share: u32, disposition: u32, options: u32) -> Result<File, NTSTATUS> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    let bytes = u16::try_from(wide.len() * 2).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let name = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_ptr() as *mut u16 };
    let attrs = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE as u32,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle: HANDLE = null_mut();
    let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let options = options | FILE_SYNCHRONOUS_IO_NONALERT;
    #[allow(clippy::disallowed_methods, reason = "one name relative to a file-system folder handle; never a pipe")]
    let s = unsafe { NtCreateFile(&mut handle, access, &attrs, &mut iosb, null(), FILE_ATTRIBUTE_NORMAL, share, disposition, options, null(), 0) };
    if s < 0 { Err(s) } else { Ok(unsafe { File::from_raw_handle(handle) }) }
}

/// A file, symlink, junction or mount point (a name-surrogate tag) is refused; other reparse points stay ordinary.
fn refuse_link(f: &File, what: &str) -> io::Result<()> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
    let ok = unsafe { GetFileInformationByHandleEx(f.as_raw_handle() as HANDLE, FileAttributeTagInfo, &mut info as *mut _ as *mut c_void, size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 || names_another_place(info.FileAttributes, info.ReparseTag) {
        return Err(invalid(format!("{what:?} is not a plain folder")));
    }
    Ok(())
}

fn status_error(s: NTSTATUS) -> io::Error {
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(s) } as i32)
}
