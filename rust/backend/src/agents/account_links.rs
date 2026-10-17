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
/// the OS refused symlinks outright (error 1314, or `Unsupported`). Then a folder becomes a junction that the daemon
/// proves it can traverse (a process enforcing redirection trust cannot cross one a non-admin made, error 448), else
/// the junction is removed and the call fails with the OS reason. A file is copied once and the account owns it after.
/// Nothing is written to the default folder, and an existing entry is the caller's to skip, so a racing sibling's
/// `AlreadyExists` passes through.
#[cfg(any(windows, test))]
pub(super) fn link_with_fallback(
    target: &Path,
    link: &Path,
    target_is_dir: bool,
    symlink: impl Fn(&Path, &Path, bool) -> io::Result<()>,
    junction: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match symlink(target, link, target_is_dir) {
        Err(err) if err.raw_os_error() == Some(PRIVILEGE_NOT_HELD) || err.kind() == io::ErrorKind::Unsupported => {
            if target_is_dir {
                junction_or_refuse(target, link, junction)
            } else {
                seed_file(target, link)
            }
        }
        other => other,
    }
}

#[cfg(any(windows, test))]
fn junction_or_refuse(target: &Path, link: &Path, junction: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    junction(target, link)?;
    if let Err(err) = std::fs::metadata(link) {
        // The reparse point only, never what it names (`remove_dir` on Windows; the Unix stand-in is a symlink).
        let _ = std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link));
        return Err(io::Error::other(format!("the junction to {} cannot be traversed: {err}", target.display())));
    }
    tracing::debug!(link = %link.display(), "account folder shared through a junction");
    Ok(())
}

/// Put the account's file in place without a reader ever seeing half of it: write a temp file beside it, then
/// `hard_link` it onto the entry name (`AlreadyExists` means a sibling won), then drop the temp.
#[cfg(any(windows, test))]
fn seed_file(target: &Path, link: &Path) -> io::Result<()> {
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = link.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    std::fs::copy(target, &temp)?;
    let made = std::fs::hard_link(&temp, link);
    let _ = std::fs::remove_file(&temp);
    made?;
    tracing::warn!(entry = %name, "symlinks are unavailable here: this account's {name} is a copy it now owns");
    Ok(())
}

/// A directory junction (no privilege needed, local NTFS only): create the folder, then set its reparse point.
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
mod tests {
    use super::super::accounts::ensure_account_links_with;
    use super::super::support_tests::{touch_dir, touch_file};
    use super::*;

    fn refused(_: &Path, _: &Path, _: bool) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(PRIVILEGE_NOT_HELD))
    }

    /// The junction maker: a symlink stands in on Unix, the real call on Windows.
    fn fake_junction(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        make_junction(target, link)
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let default = home.join(".claude");
        touch_dir(&default.join("skills").join("foo"));
        std::fs::write(default.join("CLAUDE.md"), "global rules").unwrap();
        std::fs::write(default.join("settings.json"), "{\"theme\":\"dark\"}").unwrap();
        // history.jsonl, agents, commands, plugins, output-styles, projects: absent in the default.
        let account = home.join(".claude-auth").join("team");
        touch_dir(&account);
        (tmp, account)
    }

    // Unix only: a Windows token in a redirection-trust tree cannot traverse a junction (the real-call test below).
    #[cfg(unix)]
    #[test]
    fn a_refused_symlink_becomes_a_junction_and_copies() {
        let (tmp, account) = setup();
        let home = tmp.path();
        ensure_account_links_with(home, "team", |t, l, d| link_with_fallback(t, l, d, refused, fake_junction)).unwrap();

        assert!(account.join("skills").join("foo").is_dir(), "the folder is reachable through the junction");
        assert_eq!(std::fs::read_to_string(account.join("CLAUDE.md")).unwrap(), "global rules");
        assert_eq!(std::fs::read_to_string(account.join("settings.json")).unwrap(), "{\"theme\":\"dark\"}");
        assert!(!std::fs::symlink_metadata(account.join("settings.json")).unwrap().file_type().is_symlink());
        for absent in ["history.jsonl", "agents", "commands", "plugins", "output-styles", "projects"] {
            assert!(std::fs::symlink_metadata(account.join(absent)).is_err(), "{absent}: the default lacks it");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&account)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    // Unix only: a Windows token in a redirection-trust tree cannot traverse a junction (the real-call test below).
    #[cfg(unix)]
    #[test]
    fn an_entry_already_present_is_left_untouched_by_the_fallback() {
        let (tmp, account) = setup();
        std::fs::write(account.join("settings.json"), "mine").unwrap();
        std::fs::write(account.join("CLAUDE.md"), "my rules").unwrap();
        ensure_account_links_with(tmp.path(), "team", |t, l, d| link_with_fallback(t, l, d, refused, fake_junction))
            .unwrap();
        assert_eq!(std::fs::read_to_string(account.join("settings.json")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(account.join("CLAUDE.md")).unwrap(), "my rules");
    }

    #[test]
    fn a_junction_the_daemon_cannot_traverse_is_removed_and_the_spawn_refused() {
        let (tmp, account) = setup();
        // Made fine, but it leads nowhere a read can follow: the stand-in for os error 448.
        let dangling = |_: &Path, link: &Path| fake_junction(&tmp.path().join("gone"), link);
        let err = ensure_account_links_with(tmp.path(), "team", |t, l, d| {
            link_with_fallback(t, l, d, refused, dangling)
        })
        .unwrap_err();
        assert!(err.contains("could not link"), "{err}");
        assert!(err.contains("junction"), "{err}");
        assert!(std::fs::symlink_metadata(account.join("skills")).is_err(), "the reparse point is removed");
    }

    // Hand-made junctions, symlinks, hard links and real files are all left alone, whatever the link maker would do.
    #[cfg(unix)]
    #[test]
    fn every_hand_made_entry_form_is_left_untouched() {
        let (tmp, account) = setup();
        let default = tmp.path().join(".claude");
        std::fs::write(default.join("history.jsonl"), "h").unwrap();
        touch_dir(&default.join("projects"));
        fake_junction(&default.join("skills"), &account.join("skills")).unwrap();
        std::os::unix::fs::symlink(default.join("CLAUDE.md"), account.join("CLAUDE.md")).unwrap();
        std::fs::hard_link(default.join("settings.json"), account.join("settings.json")).unwrap();
        std::fs::write(account.join("history.jsonl"), "own").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("gone"), account.join("projects")).unwrap();
        ensure_account_links_with(tmp.path(), "team", |_, _, _| panic!("no entry may be made")).unwrap();
        assert_eq!(std::fs::read_link(account.join("CLAUDE.md")).unwrap(), default.join("CLAUDE.md"));
        assert_eq!(std::fs::read_to_string(account.join("history.jsonl")).unwrap(), "own");
        assert!(std::fs::symlink_metadata(account.join("projects")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_symlink_that_works_is_used_and_another_error_is_not_a_fallback() {
        let (_tmp, account) = setup();
        let target = account.join("t");
        touch_file(&target);
        let ok = link_with_fallback(&target, &account.join("l"), false, |_, _, _| Ok(()), |_, _| panic!("no junction"));
        assert!(ok.is_ok());
        let denied = |_: &Path, _: &Path, _: bool| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let err = link_with_fallback(&target, &account.join("m"), false, denied, |_, _| panic!("no junction"));
        assert_eq!(err.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    /// The real Windows calls, whichever token runs the test: an admin links by symlink, a Basic User by junction
    /// and copy; a Basic User inside a redirection-trust tree is refused naming the OS reason, with no
    /// reparse point left behind.
    #[cfg(windows)]
    #[test]
    fn the_real_calls_either_share_every_entry_or_refuse_cleanly() {
        let (tmp, account) = setup();
        let target = tmp.path().join(".claude").join("skills");
        let probe = tmp.path().join("probe");
        match make_junction(&target, &probe) {
            Ok(()) => {
                assert!(std::fs::symlink_metadata(&probe).unwrap().file_type().is_symlink());
                assert_eq!(std::fs::read_link(&probe).unwrap(), target);
                if std::fs::metadata(&probe).is_ok() {
                    assert!(probe.join("foo").is_dir());
                }
                std::fs::remove_dir(&probe).unwrap();
                assert!(target.join("foo").is_dir(), "removing a junction leaves what it names");
            }
            Err(err) => panic!("a junction needs no privilege: {err}"),
        }
        match crate::agents::accounts::ensure_account_links(tmp.path(), "team") {
            Ok(()) => {
                assert!(account.join("skills").join("foo").is_dir());
                assert_eq!(std::fs::read_to_string(account.join("CLAUDE.md")).unwrap(), "global rules");
                assert_eq!(std::fs::read_to_string(account.join("settings.json")).unwrap(), "{\"theme\":\"dark\"}");
            }
            Err(msg) => {
                assert!(msg.contains("junction") && msg.contains("cannot be traversed"), "{msg}");
                assert!(std::fs::symlink_metadata(account.join("skills")).is_err(), "the reparse point is removed");
            }
        }
    }
}
