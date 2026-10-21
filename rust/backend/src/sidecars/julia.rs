// julia.rs — resolve the real `julia` binary the daemon spawns, shared by
// kernel.rs, repl/supervisor.rs, pluto.rs, pages/ops.rs's `run_quarto` and
// update.rs's `prepare_julia` (each keeps its own supervisor; only resolution
// is shared — one function, no privileged caller).
//
// Invariant: never spawn a PATH candidate this resolver has not verified is
// plausibly real Julia, and never a Windows app-execution alias, wherever it was
// found, not even one `SOT_JULIA_BIN` names. An alias is recognized by the file,
// not its name: on Windows the selected existing executable is inspected through
// ordinary filesystem links and refused when its reparse tag is
// `IO_REPARSE_TAG_APPEXECLINK` or when it cannot be inspected; a folder called
// `WindowsApps` is not evidence. An explicit `SOT_JULIA_BIN` is otherwise exempt
// (an operator's own choice, format-validated but not second-guessed), and a
// missing explicit absolute path is still selected and fails at spawn.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Julia's executable file name on this target, for the PATH search and the juliaup layout.
const JULIA_EXE: &str = if cfg!(windows) { "julia.exe" } else { "julia" };

/// Why no julia was selected. A caller that can live without Julia (a Windows update's prepare) skips only `NotFound`:
/// a `Refused` julia (named or found, a juliaup default that does not resolve included) fails the kernels and REPLs too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JuliaError { NotFound(String), Refused(String) }

impl std::fmt::Display for JuliaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { let (Self::NotFound(why) | Self::Refused(why)) = self; f.write_str(why) }
}

/// Resolve the `julia` binary honestly. Order:
/// 1. `SOT_JULIA_BIN`, trimmed — must not be a Windows app-execution alias
///    (a hard error), and must be an absolute path (upstream juliaup's
///    own override semantics); a non-empty but relative value is a hard
///    error rather than a silently-ignored fall-through, so a typo in an
///    explicit override is never masked by auto-detection picking something
///    else.
/// 2. juliaup's DEFAULT-CHANNEL binary, resolved by reading `juliaup.json`
///    directly (bypasses the launcher shim; an alias there is an
///    error like any other) — re-resolved on every call, so
///    a channel update or a removed version recovers on the very next spawn
///    attempt rather than replaying a permanently cached path.
/// 3. A PATH candidate, rejected (and the search continued to the next
///    entry) if it is not a real executable (Unix: the executable bit is
///    unset) or is a Windows Store app-execution alias. If no candidate exists, or every one found is
///    rejected, this is an error: a bare `"julia"` is never offered, since
///    the OS would resolve the same rejected file (and Windows resolves a
///    bare name to the Store alias).
pub(crate) fn resolve_bin() -> Result<(String, &'static str), JuliaError> {
    resolve_bin_on(std::env::var_os("PATH").as_deref())
}

/// [`resolve_bin`] with the `PATH` value passed in: the one place the PATH search reads it, so a test supplies its own
/// and never changes the process's. Whatever [`find_on`] finds is checked once more here, so no answer is an
/// app-execution alias, wherever it was found (a juliaup version's `Path` may point anywhere); the check reads the
/// file, through ordinary filesystem links.
fn resolve_bin_on(path: Option<&std::ffi::OsStr>) -> Result<(String, &'static str), JuliaError> {
    let (bin, source) = find_on(path)?;
    if let Some(why) = alias_refusal(Path::new(&bin)) {
        return Err(JuliaError::Refused(format!("{bin:?} ({source}) {why}")));
    }
    Ok((bin, source))
}

/// The search itself: the override, juliaup's default channel, then the PATH candidates.
fn find_on(path: Option<&std::ffi::OsStr>) -> Result<(String, &'static str), JuliaError> {
    if let Some(v) = std::env::var_os("SOT_JULIA_BIN") {
        let trimmed = v.to_string_lossy().trim().to_string();
        if !trimmed.is_empty() {
            if let Some(why) = alias_refusal(Path::new(&trimmed)) {
                return Err(JuliaError::Refused(format!("SOT_JULIA_BIN={trimmed:?} {why}")));
            }
            return if Path::new(&trimmed).is_absolute() {
                Ok((trimmed, "SOT_JULIA_BIN"))
            } else {
                Err(JuliaError::Refused(format!("SOT_JULIA_BIN={trimmed:?} is not an absolute path (juliaup requires absolute overrides)")))
            };
        }
    }
    let juliaup = home_dir().map_or(Ok(None), |home| resolve_juliaup_julia(&home));
    match &juliaup {
        Ok(Some(bin)) => return Ok((bin.to_string_lossy().into_owned(), "juliaup")),
        Err(e) => tracing::warn!(error = %e, "juliaup present but its layout didn't parse as expected; falling back to PATH"),
        Ok(None) => {}
    }
    let candidates = candidates_on(path, JULIA_EXE);
    if candidates.is_empty() {
        let none = "no `julia` found: SOT_JULIA_BIN unset, no juliaup default channel, none on PATH";
        return Err(match juliaup {
            Err(e) => JuliaError::Refused(format!("{none} (juliaup's default channel did not resolve: {e})")),
            Ok(_) => JuliaError::NotFound(none.into()),
        });
    }
    let mut first_rejection = None;
    for candidate in candidates {
        match looks_like_fake_julia(&candidate) {
            None => return Ok((candidate.to_string_lossy().into_owned(), "PATH")),
            Some(reason) => {
                tracing::warn!(
                    path = %candidate.display(),
                    reason = %reason,
                    "rejected PATH `julia` candidate; trying the next one"
                );
                first_rejection.get_or_insert_with(|| format!("{}: {reason}", candidate.display()));
            }
        }
    }
    Err(JuliaError::Refused(format!("every `julia` on PATH was rejected (bare `julia` would resolve the same one) — {}", first_rejection.unwrap_or_default())))
}

fn home_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    if cfg!(windows) {
        if let Some(u) = std::env::var_os("USERPROFILE") {
            if !u.is_empty() {
                return Some(PathBuf::from(u));
            }
        }
    }
    None
}

/// juliaup's own home directory: `juliaup.json` plus the versioned install dirs. An explicit `JULIAUP_DEPOT_PATH`
/// names the depot root (storage at `<root>/juliaup`); otherwise `~/.julia` REGARDLESS of `JULIA_DEPOT_PATH`, since
/// juliaup manages its toolchains apart from the running Julia's package depot (upstream `src/global_paths.rs`).
fn juliaup_home_dir(home: &Path) -> PathBuf {
    match std::env::var_os("JULIAUP_DEPOT_PATH") {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("juliaup"),
        _ => home.join(".julia").join("juliaup"),
    }
}

/// Read juliaup's `juliaup.json` and resolve its DEFAULT channel to a real
/// `bin/julia[.exe]` path (see `juliaup_version_binary`), bypassing juliaup's own launcher shim entirely.
/// `Ok(None)` means "juliaup isn't installed here" (no config file): a
/// normal, silent case, never a failure. `Err` means a config WAS found but
/// didn't parse the way juliaup documents its own layout — worth a warning;
/// the caller still falls through to PATH.
fn resolve_juliaup_julia(home: &Path) -> Result<Option<PathBuf>, String> {
    let juliaup_dir = juliaup_home_dir(home);
    let config_path = juliaup_dir.join("juliaup.json");
    let text = match std::fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", config_path.display())),
    };
    resolve_juliaup_julia_from_config(&juliaup_dir, &text).map(Some)
}

/// Pure parse-and-resolve step, split out from the filesystem read so tests
/// can hand it a fixed JSON string plus a fake `juliaup_dir` instead of
/// needing a real `$HOME`. juliaup.json shape (observed on a real install):
/// ```json
/// { "Default": "release",
///   "InstalledChannels": { "release": { "Version": "1.12.6+0.x64.linux.gnu" } },
///   "InstalledVersions": { "1.12.6+0.x64.linux.gnu": { "Path": "./julia-1.12.6+0.x64.linux.gnu" } } }
/// ```
/// Deliberately ignores juliaup's per-directory `Overrides` — this resolves
/// the DEFAULT channel only, matching what the daemon needs (one global
/// kernel/REPL/Pluto binary, not per-project channel pinning).
fn resolve_juliaup_julia_from_config(
    juliaup_dir: &Path,
    config_text: &str,
) -> Result<PathBuf, String> {
    let config: Value =
        serde_json::from_str(config_text).map_err(|e| format!("juliaup.json: {e}"))?;
    let default_channel = config
        .get("Default")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "juliaup.json has no `Default` channel".to_string())?;
    let version = config
        .get("InstalledChannels")
        .and_then(|c| c.get(default_channel))
        .and_then(|c| c.get("Version"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("default channel {default_channel:?} not in `InstalledChannels`"))?;
    let rel_path = config
        .get("InstalledVersions")
        .and_then(|v| v.get(version))
        .and_then(|v| v.get("Path"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| format!("version {version:?} not in `InstalledVersions`"))?;
    // juliaup writes `Path` as `./julia-<version>`; strip the `./` so the joined path reads cleanly in logs.
    let rel_path = rel_path.strip_prefix("./").unwrap_or(rel_path);
    let bin = juliaup_version_binary(&juliaup_dir.join(rel_path));
    if is_present_file(&bin) {
        Ok(bin)
    } else {
        Err(format!("resolved binary missing on disk: {}", bin.display()))
    }
}

/// The Julia binary in a juliaup version directory, as juliaup resolves it: on macOS a DMG install is an app bundle,
/// `<name>.app/Contents/Resources/julia/bin/julia` (the first `.app`, in directory order, holding the binary wins), else `bin/julia`.
/// Not OS-gated, so a Linux test exercises the scan; with nothing present it names `bin/julia` for the error.
fn juliaup_version_binary(version_dir: &Path) -> PathBuf {
    let apps = std::fs::read_dir(version_dir).into_iter().flatten().flatten().map(|e| e.path());
    let bundled = apps.filter(|p| p.extension().is_some_and(|x| x == "app"));
    let mut bins = bundled.map(|app| app.join("Contents/Resources/julia/bin").join(JULIA_EXE));
    bins.find(|b| is_present_file(b)).unwrap_or_else(|| version_dir.join("bin").join(JULIA_EXE))
}

/// Why the file at `path` must not be spawned as Julia because it is a Windows app-execution alias, or `None`.
/// Only an existing file is inspected (a missing explicit path is the spawn's error to report). On Windows the file
/// is followed through ordinary filesystem links, with a bound, and refused when its terminal reparse tag is
/// `IO_REPARSE_TAG_APPEXECLINK` (a Store placeholder that runs outside the daemon's containment, ADR 0050 residual 7)
/// or when it cannot be inspected; elsewhere there are no aliases.
fn alias_refusal(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        match alias::is_app_execution_alias(path) {
            Ok(false) => None,
            Ok(true) => Some(
                "is a Windows app-execution alias; a julia started through one runs outside the daemon's containment \
                 (ADR 0050 residual 7)"
                    .to_string(),
            ),
            Err(e) => Some(format!("could not be inspected for an app-execution alias: {e}")),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
mod alias {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    /// The reparse tag of a Store app-execution alias.
    const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
    /// The reparse tag of a symbolic link.
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    /// `FILE_READ_ATTRIBUTES`: the handle can be queried and nothing is read or run.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    /// Links followed before a path is called a cycle.
    const MAX_LINKS: usize = 32;

    /// The terminal reparse tag of the file at `path`, queried as a reparse point so an alias is not activated;
    /// `None` for a file that is not one. Directories in front of the leaf (junctions included) are resolved by the
    /// system as usual.
    fn reparse_tag(path: &Path) -> std::io::Result<Option<u32>> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(7)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let mut info = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
        // SAFETY: the handle is open for the file's life and `info` is the size the class names.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileAttributeTagInfo,
                (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(info.ReparseTag))
    }

    /// Whether the existing file at `path`, followed through symbolic links, is an app-execution alias. A missing
    /// file is not one; any other failure to inspect an existing file is an error, as is a link cycle.
    pub(super) fn is_app_execution_alias(path: &Path) -> std::io::Result<bool> {
        let mut current: PathBuf = path.to_path_buf();
        for _ in 0..MAX_LINKS {
            let tag = match reparse_tag(&current) {
                Ok(tag) => tag,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e),
            };
            match tag {
                None => return Ok(false),
                Some(IO_REPARSE_TAG_APPEXECLINK) => return Ok(true),
                Some(IO_REPARSE_TAG_SYMLINK) => {
                    let target = std::fs::read_link(&current)?;
                    current = if target.is_absolute() {
                        target
                    } else {
                        current.parent().unwrap_or_else(|| Path::new("")).join(target)
                    };
                }
                Some(_) => return Ok(false),
            }
        }
        Err(std::io::Error::other("too many links"))
    }

    /// Reparse tag of the file at `path` itself, for tests that look for a real alias.
    #[cfg(test)]
    pub(super) fn tag_of(path: &Path) -> std::io::Result<Option<u32>> {
        reparse_tag(path)
    }

    #[cfg(test)]
    pub(super) const APPEXECLINK: u32 = IO_REPARSE_TAG_APPEXECLINK;
}

/// A candidate `julia`/`julia.exe` found on PATH that must be rejected
/// before the daemon spawns it, with the reason:
/// - Unix: not executable (the field failure's shape under a different
///   name — any placeholder lacking the exec bit).
/// - Windows: a Store app-execution alias — PATH commonly resolves
///   `julia.exe` to `...\WindowsApps\julia.exe` for a user who has never
///   run `julia` interactively (the Store's placeholder for "you don't
///   have this app, want to install it?"). It is a reparse-point stub;
///   spawned by a detached daemon (no interactive shell to field the Store
///   prompt) it exits at once with no output — the exact field failure.
#[cfg(unix)]
fn looks_like_fake_julia(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) if md.permissions().mode() & 0o111 == 0 => Some("not executable".to_string()),
        Ok(_) => None,
        Err(_) => Some("metadata unreadable".to_string()),
    }
}

#[cfg(windows)]
fn looks_like_fake_julia(path: &Path) -> Option<String> {
    alias_refusal(path)
}

/// Whether a file is there to be chosen. On Windows a link to an app-execution alias cannot be followed by a
/// metadata query, so `is_file` calls it missing and the alias would be skipped unseen instead of refused; the
/// link itself, not what it leads to, is what is looked for.
fn is_present_file(path: &Path) -> bool {
    if cfg!(windows) {
        std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir())
    } else {
        path.is_file()
    }
}

/// Every `julia`/`julia.exe` reachable via `PATH`, in PATH order, without
/// relying on the OS's own single-candidate lookup (`Command::new` spawning
/// straight off PATH can't skip a rejected first match and try the next
/// entry — this can).
fn candidates_on(path: Option<&std::ffi::OsStr>, exe_name: &str) -> Vec<PathBuf> {
    path
        .map(|p| {
            std::env::split_paths(p)
                .map(|dir| dir.join(exe_name))
                .filter(|c| is_present_file(c))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "julia_tests.rs"]
mod tests;
