//! What a named account gets where the daemon cannot make a symbolic link (Windows without Developer Mode or the
//! symlink privilege): a junction for a folder, and for a file a one-time copy the account then owns.

#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::path::Path;

/// `ERROR_PRIVILEGE_NOT_HELD`: the OS refusal a symlink gets without Developer Mode or the symlink privilege.
#[cfg(any(windows, test))]
const PRIVILEGE_NOT_HELD: i32 = 1314;

/// One shared entry for a daemon whose symlink may be refused. `symlink` is tried first and its result stands, unless
/// the OS refused symlinks outright (error 1314). Then a folder becomes a junction that the daemon proves it can
/// traverse (a process enforcing redirection trust cannot cross one a non-admin made, error 448), else the call fails
/// with the OS reason. A file is copied once and the account owns it after. Each entry is built under a name unique to
/// the call and put in place without replacing anything, so the real name only ever holds a complete entry and a
/// sibling spawn's `AlreadyExists` always means one is there. Nothing is written to the default folder.
#[cfg(any(windows, test))]
pub(super) fn link_with_fallback(
    target: &Path,
    link: &Path,
    target_is_dir: bool,
    symlink: impl Fn(&Path, &Path, bool) -> io::Result<()>,
    junction: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match symlink(target, link, target_is_dir) {
        Err(err) if err.raw_os_error() == Some(PRIVILEGE_NOT_HELD) => {
            if target_is_dir {
                junction_or_refuse(target, link, junction)
            } else {
                seed_file(target, link)
            }
        }
        other => other,
    }
}

/// `link` with a name no other call in any process shares: the pid, a per-process counter and the entry's own name.
#[cfg(any(windows, test))]
fn unique_beside(link: &Path) -> std::path::PathBuf {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    link.with_file_name(format!(".{name}.{}-{call}.tmp", std::process::id()))
}

/// Remove this call's own temp junction: the reparse point only, never what it names (`remove_dir` on Windows; the
/// Unix stand-in is a symlink).
#[cfg(any(windows, test))]
fn remove_own(temp: &Path) {
    let _ = std::fs::remove_dir(temp).or_else(|_| std::fs::remove_file(temp));
}

#[cfg(any(windows, test))]
fn junction_or_refuse(target: &Path, link: &Path, junction: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let temp = unique_beside(link);
    // Whatever stops the call's own temp from being made is never "the entry is there": only the publish says that.
    if let Err(err) = junction(target, &temp) {
        return Err(io::Error::other(format!("could not make the junction {}: {err}", temp.display())));
    }
    if let Err(err) = std::fs::metadata(&temp) {
        remove_own(&temp);
        return Err(io::Error::other(format!("the junction to {} cannot be traversed: {err}", target.display())));
    }
    if let Err(err) = publish_dir(&temp, link) {
        remove_own(&temp);
        return Err(err);
    }
    tracing::debug!(link = %link.display(), "account folder shared through a junction");
    Ok(())
}

/// Move a verified junction to its real name, failing with `AlreadyExists` when anything is there.
#[cfg(windows)]
fn publish_dir(temp: &Path, link: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (from, to) = (wide(temp), wide(link));
    // SAFETY: both buffers are NUL-terminated and outlive the call. Flags 0: no replace, no copy across volumes.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// The Unix stand-in's publish: the temp is a symlink, and `symlink` fails on an existing name.
#[cfg(all(unix, test))]
fn publish_dir(temp: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(std::fs::read_link(temp)?, link)?;
    remove_own(temp);
    Ok(())
}

/// How a named account's `CLAUDE.md` is seeded where symlinks are refused: `false` is a one-time copy the account owns
/// (rule S); `true` is a one-line import stub, `@<the default's CLAUDE.md>`, so the account follows the default's edits.
/// The one line to change once the import form is confirmed to work for a named account.
#[cfg(any(windows, test))]
const CLAUDE_MD_AS_IMPORT_STUB: bool = false;

/// The stub's one line, in the form Claude Code's import reader takes: `@~/.claude/CLAUDE.md`, for the default's file only,
/// `<home>/.claude/CLAUDE.md` as the daemon builds it from the home it resolves (the reader ends a match at a backslash or a
/// space, so a Windows path never imports as written, and this spelling has no path to get right). `None` for any other
/// target, even one in a folder named `.claude` elsewhere: it names another file, so it gets the copy.
#[cfg(any(windows, test))]
fn import_line(target: &Path, home: &Path) -> Option<&'static str> {
    (target == super::accounts::claude_config_dir(home, "").join("CLAUDE.md")).then_some("@~/.claude/CLAUDE.md\n")
}

/// Whether `link` takes the import-stub form under `switch`: only the `CLAUDE.md`.
#[cfg(any(windows, test))]
fn uses_import_stub(switch: bool, link: &Path) -> bool {
    switch && link.file_name().is_some_and(|n| n == "CLAUDE.md")
}

#[cfg(any(windows, test))]
fn seed_file(target: &Path, link: &Path) -> io::Result<()> {
    seed_with(target, link, CLAUDE_MD_AS_IMPORT_STUB, super::accounts::account_home().as_deref())
}

/// Seed `link` from `target` under `switch`: the import stub when the switch is on, the entry is `CLAUDE.md` and the target is
/// the default's under `home` (`import_line`), else a copy.
#[cfg(any(windows, test))]
fn seed_with(target: &Path, link: &Path, switch: bool, home: Option<&Path>) -> io::Result<()> {
    let line = home.filter(|_| uses_import_stub(switch, link)).and_then(|h| import_line(target, h));
    seed_contents(target, link, line)?;
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if line.is_some() {
        tracing::warn!(entry = %name, "symlinks are unavailable here: this account's {name} imports the default's");
    } else {
        tracing::warn!(entry = %name, "symlinks are unavailable here: this account's {name} is a copy it now owns");
    }
    Ok(())
}

/// Put the account's file in place without a reader ever seeing half of it: write a temp file beside it (a copy of
/// `target`, or with `stub` the one-line import of it), then `hard_link` it onto the entry name (`AlreadyExists` means
/// a sibling won), then drop the temp.
#[cfg(any(windows, test))]
fn seed_contents(target: &Path, link: &Path, stub: Option<&str>) -> io::Result<()> {
    let temp = unique_beside(link);
    #[allow(clippy::disallowed_methods, reason = "a data copy of an account's file; the link's target is never a program this process runs")]
    let (written, what) = if let Some(line) = stub {
        (std::fs::write(&temp, line), "write the import of")
    } else {
        (std::fs::copy(target, &temp).map(|_| ()), "copy")
    };
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(io::Error::other(format!("could not {what} {} to {}: {err}", target.display(), temp.display())));
    }
    let made = std::fs::hard_link(&temp, link);
    let _ = std::fs::remove_file(&temp);
    made
}

/// A directory junction (no privilege needed, local NTFS only) at `link`, a name the caller owns: create the folder,
/// then set its reparse point.
#[cfg(windows)]
pub(super) fn make_junction(target: &Path, link: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    // The reparse buffer wants the NT spelling `\??\C:\..`; a verbatim `\\?\` prefix is dropped first.
    let wide: Vec<u16> = target.as_os_str().encode_wide().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let print: Vec<u16> = wide.strip_prefix(verbatim.as_slice()).unwrap_or(&wide).to_vec();
    let subst: Vec<u16> = r"\??\".encode_utf16().chain(print.iter().copied()).collect();
    let data_len = u16::try_from(8 + (subst.len() + 1 + print.len() + 1) * 2)
        .map_err(|_| io::Error::other("junction target path too long"))?;
    let mut buf: Vec<u8> = Vec::with_capacity(8 + usize::from(data_len));
    for half in [
        (IO_REPARSE_TAG_MOUNT_POINT & 0xFFFF) as u16,
        (IO_REPARSE_TAG_MOUNT_POINT >> 16) as u16,
        data_len,
        0,
        0,
        (subst.len() * 2) as u16,
        ((subst.len() + 1) * 2) as u16,
        (print.len() * 2) as u16,
    ] {
        buf.extend_from_slice(&half.to_le_bytes());
    }
    for w in subst.iter().chain(&[0]).chain(print.iter()).chain(&[0]) {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    std::fs::create_dir(link)?;
    let set = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .and_then(|dir| {
            let mut returned = 0u32;
            // SAFETY: `buf` is a complete REPARSE_DATA_BUFFER that outlives the call; there is no output buffer.
            let ok = unsafe {
                DeviceIoControl(
                    dir.as_raw_handle(),
                    FSCTL_SET_REPARSE_POINT,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    if set.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    set
}

#[cfg(test)]
#[path = "account_links_tests.rs"]
mod tests;
