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
pub(crate) fn resolve_bin() -> Result<(String, &'static str), String> {
    resolve_bin_on(std::env::var_os("PATH").as_deref())
}

/// [`resolve_bin`] with the `PATH` value passed in: the one place the PATH search reads it, so a test supplies its own
/// and never changes the process's. Whatever [`find_on`] finds is checked once more here, so no answer is an
/// app-execution alias, wherever it was found (a juliaup version's `Path` may point anywhere); the check reads the
/// file, through ordinary filesystem links.
fn resolve_bin_on(path: Option<&std::ffi::OsStr>) -> Result<(String, &'static str), String> {
    let (bin, source) = find_on(path)?;
    if let Some(why) = alias_refusal(Path::new(&bin)) {
        return Err(format!("{bin:?} ({source}) {why}"));
    }
    Ok((bin, source))
}

/// The search itself: the override, juliaup's default channel, then the PATH candidates.
fn find_on(path: Option<&std::ffi::OsStr>) -> Result<(String, &'static str), String> {
    if let Some(v) = std::env::var_os("SOT_JULIA_BIN") {
        let trimmed = v.to_string_lossy().trim().to_string();
        if !trimmed.is_empty() {
            if let Some(why) = alias_refusal(Path::new(&trimmed)) {
                return Err(format!("SOT_JULIA_BIN={trimmed:?} {why}"));
            }
            return if Path::new(&trimmed).is_absolute() {
                Ok((trimmed, "SOT_JULIA_BIN"))
            } else {
                Err(format!(
                    "SOT_JULIA_BIN={trimmed:?} is not an absolute path (juliaup requires \
                     absolute overrides)"
                ))
            };
        }
    }
    if let Some(home) = home_dir() {
        match resolve_juliaup_julia(&home) {
            Ok(Some(path)) => return Ok((path.to_string_lossy().into_owned(), "juliaup")),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                error = %e,
                "juliaup present but its layout didn't parse as expected; falling back to PATH"
            ),
        }
    }
    let candidates = candidates_on(path, JULIA_EXE);
    if candidates.is_empty() {
        return Err("no `julia` found: SOT_JULIA_BIN unset, no juliaup default channel, none on PATH".into());
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
    Err(format!(
        "every `julia` on PATH was rejected (bare `julia` would resolve the same one) — {}",
        first_rejection.unwrap_or_default()
    ))
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

/// juliaup's own home directory: `juliaup.json` plus the versioned install
/// dirs. An explicit `JULIAUP_DEPOT_PATH` names the depot root (juliaup's
/// storage lives at `<root>/juliaup`); otherwise juliaup uses the default
/// Julia depot `~/.julia` REGARDLESS of `JULIA_DEPOT_PATH` — juliaup manages
/// its own toolchain installs independently of whatever depot the running
/// Julia process uses for packages (matches upstream:
/// https://github.com/JuliaLang/juliaup/blob/main/src/global_paths.rs).
fn juliaup_home_dir(home: &Path) -> PathBuf {
    match std::env::var_os("JULIAUP_DEPOT_PATH") {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("juliaup"),
        _ => home.join(".julia").join("juliaup"),
    }
}

/// Read juliaup's `juliaup.json` and resolve its DEFAULT channel to a real
/// `bin/julia[.exe]` path, bypassing juliaup's own launcher shim entirely.
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
    // juliaup writes `Path` as `./julia-<version>` (leading `./`); strip it
    // so the joined path reads cleanly in logs (`PathBuf::join` doesn't
    // normalize a literal `./` component away).
    let rel_path = rel_path.strip_prefix("./").unwrap_or(rel_path);
    let bin = juliaup_dir.join(rel_path).join("bin").join(JULIA_EXE);
    if is_present_file(&bin) {
        Ok(bin)
    } else {
        Err(format!("resolved binary missing on disk: {}", bin.display()))
    }
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
mod tests {
    use super::*;
    use crate::paths::{EnvGuard, ENV_TEST_LOCK};

    // --- looks_like_fake_julia (Unix: executable-bit check) ---------------

    #[cfg(unix)]
    #[test]
    fn rejects_a_non_executable_file_regardless_of_size() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("julia");
        // Deliberately LARGE (proves the old 4 KiB rule is gone — size is
        // no longer part of the check at all): only the exec bit matters.
        std::fs::write(&path, vec![0u8; 64 * 1024]).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(looks_like_fake_julia(&path).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn accepts_a_tiny_executable_file() {
        // Proves the old size floor is gone from the other direction too:
        // a small but genuinely executable file is accepted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("julia");
        sot_log::test_exec::write_executable(&path, b"#!/bin/sh\nexit 0\n");
        assert_eq!(looks_like_fake_julia(&path), None);
    }

    // --- resolve_juliaup_julia_from_config (table test) --------------------

    fn write_fake_juliaup_home(juliaup_dir: &Path, version_dir_name: &str) {
        let bin_dir = juliaup_dir.join(version_dir_name).join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
        sot_log::test_exec::write_executable(&bin_dir.join(exe_name), vec![0u8; 8192]);
    }

    #[test]
    fn resolves_the_default_channel_real_binary() {
        let dir = tempfile::tempdir().unwrap();
        let juliaup_dir = dir.path();
        write_fake_juliaup_home(juliaup_dir, "julia-1.12.6+0.x64.linux.gnu");
        let config = r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.12.6+0.x64.linux.gnu": { "Path": "./julia-1.12.6+0.x64.linux.gnu" } }
        }"#;
        let resolved = resolve_juliaup_julia_from_config(juliaup_dir, config).unwrap();
        let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
        assert_eq!(
            resolved,
            juliaup_dir.join("julia-1.12.6+0.x64.linux.gnu").join("bin").join(exe_name)
        );
    }

    #[test]
    fn juliaup_config_parse_failures_report_a_useful_reason() {
        let dir = tempfile::tempdir().unwrap();
        let cases: &[(&str, &str, &str)] = &[
            ("malformed json", "not json", "juliaup.json"),
            ("missing Default", r#"{"InstalledChannels":{}}"#, "Default"),
            (
                "channel not in InstalledChannels",
                r#"{"Default":"release","InstalledChannels":{}}"#,
                "InstalledChannels",
            ),
            (
                "version not in InstalledVersions",
                r#"{"Default":"release","InstalledChannels":{"release":{"Version":"9.9.9+0"}},"InstalledVersions":{}}"#,
                "InstalledVersions",
            ),
            (
                "resolved binary missing on disk",
                r#"{"Default":"release","InstalledChannels":{"release":{"Version":"1.12.6+0.x64.linux.gnu"}},"InstalledVersions":{"1.12.6+0.x64.linux.gnu":{"Path":"./julia-1.12.6+0.x64.linux.gnu"}}}"#,
                "missing on disk",
            ),
        ];
        for (label, config, expect_substr) in cases {
            let err = resolve_juliaup_julia_from_config(dir.path(), config).unwrap_err();
            assert!(err.contains(expect_substr), "{label}: unexpected message: {err}");
        }
    }

    // --- resolve_bin (full precedence chain, env-mutating) ------------------

    #[test]
    fn env_guard_restores_a_set_variable_and_unsets_an_unset_one() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const KEY: &str = "SOT_MW14_GUARD_PIN";
        std::env::set_var(KEY, "before");
        {
            let _g = EnvGuard::capture(KEY);
            std::env::set_var(KEY, "during");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("during"));
        }
        assert_eq!(std::env::var(KEY).as_deref(), Ok("before"));
        std::env::remove_var(KEY);
        {
            let _g = EnvGuard::capture(KEY);
            std::env::set_var(KEY, "x");
        }
        assert!(std::env::var_os(KEY).is_none());
    }

    #[test]
    fn sot_julia_bin_wins_outright_when_absolute() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        // An absolute path on THIS platform (a Unix-style "/x/y" is not
        // absolute on Windows and fails the juliaup rule there).
        let abs = std::env::temp_dir().join("explicit-override-julia");
        let abs_str = abs.to_string_lossy().into_owned();
        std::env::set_var("SOT_JULIA_BIN", format!("  {abs_str}  "));
        std::env::set_var("HOME", "/should/not/matter");
        let (bin, source) = resolve_bin().unwrap();
        assert_eq!(bin, abs_str); // trimmed
        assert_eq!(source, "SOT_JULIA_BIN");
    }

    #[test]
    fn sot_julia_bin_relative_is_a_hard_error() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        std::env::set_var("SOT_JULIA_BIN", "relative/julia");
        let err = resolve_bin().unwrap_err();
        assert!(err.contains("absolute"), "unexpected message: {err}");
    }

    #[test]
    fn falls_back_to_juliaup_default_channel_when_no_override() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let juliaup_dir = home.path().join(".julia").join("juliaup");
        std::fs::create_dir_all(&juliaup_dir).unwrap();
        write_fake_juliaup_home(&juliaup_dir, "julia-1.12.6+0.x64.linux.gnu");
        let config = r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.12.6+0.x64.linux.gnu": { "Path": "./julia-1.12.6+0.x64.linux.gnu" } }
        }"#;
        std::fs::write(juliaup_dir.join("juliaup.json"), config).unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");

        let (bin, source) = resolve_bin().unwrap();
        assert_eq!(source, "juliaup");
        let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
        assert_eq!(
            bin,
            juliaup_dir.join("julia-1.12.6+0.x64.linux.gnu").join("bin").join(exe_name).to_string_lossy()
        );
    }

    #[test]
    fn juliaup_depot_path_override_relocates_the_search() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let depot_root = tempfile::tempdir().unwrap();
        let juliaup_dir = depot_root.path().join("juliaup");
        std::fs::create_dir_all(&juliaup_dir).unwrap();
        write_fake_juliaup_home(&juliaup_dir, "julia-1.10.11+0.x64.linux.gnu");
        let config = r#"{
            "Default": "1.10",
            "InstalledChannels": { "1.10": { "Version": "1.10.11+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.10.11+0.x64.linux.gnu": { "Path": "./julia-1.10.11+0.x64.linux.gnu" } }
        }"#;
        std::fs::write(juliaup_dir.join("juliaup.json"), config).unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        let unrelated_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", unrelated_home.path());
        std::env::set_var("JULIAUP_DEPOT_PATH", depot_root.path());

        let (bin, source) = resolve_bin().unwrap();
        assert_eq!(source, "juliaup");
        assert!(
            PathBuf::from(&bin).starts_with(&juliaup_dir),
            "expected {bin} to resolve under the overridden depot {juliaup_dir:?}"
        );
    }

    #[test]
    fn falls_back_to_path_when_juliaup_absent() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap(); // no .julia/juliaup here
        let path_dir = tempfile::tempdir().unwrap();
        let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
        let real_julia = path_dir.path().join(exe_name);
        sot_log::test_exec::write_executable(&real_julia, vec![0u8; 8192]);

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        
        let (bin, source) = resolve_bin_on(Some(path_dir.path().as_os_str())).unwrap();
        assert_eq!(source, "PATH");
        assert_eq!(bin, real_julia.to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn path_search_skips_a_non_executable_candidate_for_a_real_one_further_along() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let fake_dir = tempfile::tempdir().unwrap();
        let real_dir = tempfile::tempdir().unwrap();
        std::fs::write(fake_dir.path().join("julia"), b"not executable").unwrap();
        let real_julia = real_dir.path().join("julia");
        sot_log::test_exec::write_executable(&real_julia, vec![0u8; 8192]);

        let joined_path = std::env::join_paths([fake_dir.path(), real_dir.path()]).unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        
        let (bin, source) = resolve_bin_on(Some(joined_path.as_os_str())).unwrap();
        assert_eq!(source, "PATH");
        assert_eq!(bin, real_julia.to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn every_path_candidate_rejected_is_an_error_not_a_bare_name_fallback() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let path_dir = tempfile::tempdir().unwrap();
        std::fs::write(path_dir.path().join("julia"), b"not executable").unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        
        let err = resolve_bin_on(Some(path_dir.path().as_os_str())).unwrap_err();
        assert!(err.contains("rejected"), "unexpected message: {err}");
    }

    #[test]
    fn no_julia_anywhere_is_an_error_not_a_bare_name() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let empty_path_dir = tempfile::tempdir().unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");

        let err = resolve_bin_on(Some(empty_path_dir.path().as_os_str())).unwrap_err();
        assert!(err.contains("no `julia` found"), "unexpected message: {err}");
    }

    /// A directory that happens to be called `WindowsApps` (any case) holds an ordinary executable: the name is not
    /// evidence of an alias, so the override, the juliaup answer and the PATH search all accept it.
    #[test]
    fn ordinary_windowsapps_executable_is_accepted() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");

        for folder in ["WindowsApps", "windowsapps"] {
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join(folder).join(JULIA_EXE);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            sot_log::test_exec::write_executable(&exe, vec![0u8; 8192]);
            let exe_str = exe.to_string_lossy().into_owned();

            std::env::set_var("SOT_JULIA_BIN", &exe);
            assert_eq!(resolve_bin_on(Some(std::ffi::OsStr::new(""))), Ok((exe_str.clone(), "SOT_JULIA_BIN")), "{folder}: override");

            std::env::remove_var("SOT_JULIA_BIN");
            let path = std::env::join_paths([exe.parent().unwrap()]).unwrap();
            assert_eq!(resolve_bin_on(Some(path.as_os_str())), Ok((exe_str, "PATH")), "{folder}: PATH");
        }

        // A juliaup version kept under such a folder.
        let juliaup_dir = home.path().join(".julia").join("juliaup");
        std::fs::create_dir_all(&juliaup_dir).unwrap();
        let version_dir = home.path().join("WindowsApps").join("jl");
        std::fs::create_dir_all(version_dir.join("bin")).unwrap();
        sot_log::test_exec::write_executable(&version_dir.join("bin").join(JULIA_EXE), vec![0u8; 8192]);
        let config = serde_json::json!({
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6" } },
            "InstalledVersions": { "1.12.6": { "Path": version_dir.to_string_lossy() } },
        });
        std::fs::write(juliaup_dir.join("juliaup.json"), config.to_string()).unwrap();
        let (bin, source) = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap();
        assert_eq!(source, "juliaup");
        assert_eq!(bin, version_dir.join("bin").join(JULIA_EXE).to_string_lossy());
    }

    /// The real-alias cases (Windows): a runner-owned app-execution alias, reached by spellings with no `WindowsApps`
    /// component. The alias is found, never faked: none present is a setup failure.
    #[cfg(windows)]
    mod real_alias {
        use super::*;

        /// An app-execution alias of the account, confirmed by its reparse tag.
        fn real_alias() -> PathBuf {
            let apps = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("setup: LOCALAPPDATA"))
                .join("Microsoft")
                .join("WindowsApps");
            std::fs::read_dir(&apps)
                .unwrap_or_else(|e| panic!("setup: {} unreadable: {e}", apps.display()))
                .flatten()
                .map(|entry| entry.path())
                .find(|p| alias::tag_of(p).ok().flatten() == Some(alias::APPEXECLINK))
                .expect("setup: no app-execution alias on this runner")
        }

        /// An owned junction to the alias's folder and an owned `julia.exe` link to the alias: neither spelling has a
        /// `WindowsApps` component.
        struct Spellings {
            _dir: tempfile::TempDir,
            via_junction: PathBuf,
            via_link: PathBuf,
            link_dir: PathBuf,
        }

        fn spellings() -> Spellings {
            let alias = real_alias();
            let dir = tempfile::tempdir().unwrap();
            let junction = dir.path().join("j1");
            let mut mklink = std::process::Command::new("cmd");
            mklink.args(["/C", "mklink", "/J"]).arg(&junction).arg(alias.parent().unwrap());
            let made = crate::lifecycle::child_signal::process().output(&mut mklink).expect("run mklink");
            assert!(made.status.success(), "setup: the owned junction could not be made");
            let link_dir = dir.path().join("links");
            std::fs::create_dir_all(&link_dir).unwrap();
            let via_link = link_dir.join(JULIA_EXE);
            std::os::windows::fs::symlink_file(&alias, &via_link).expect("setup: the owned file link could not be made");
            Spellings {
                via_junction: junction.join(alias.file_name().unwrap()),
                via_link,
                link_dir,
                _dir: dir,
            }
        }

        #[test]
        fn alternate_alias_override_is_refused() {
            let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let _g = EnvGuard::capture("SOT_JULIA_BIN");
            let sp = spellings();
            for candidate in [&sp.via_junction, &sp.via_link] {
                std::env::set_var("SOT_JULIA_BIN", candidate);
                let err = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap_err();
                assert!(err.contains("app-execution alias"), "{}: {err}", candidate.display());
            }
        }

        #[test]
        fn alternate_alias_juliaup_is_refused() {
            let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let home = tempfile::tempdir().unwrap();
            let sp = spellings();
            let juliaup_dir = home.path().join(".julia").join("juliaup");
            std::fs::create_dir_all(&juliaup_dir).unwrap();
            let version_dir = sp.link_dir.parent().unwrap().join("jl");
            std::fs::create_dir_all(version_dir.join("bin")).unwrap();
            std::os::windows::fs::symlink_file(&sp.via_link, version_dir.join("bin").join(JULIA_EXE)).unwrap();
            let config = serde_json::json!({
                "Default": "release",
                "InstalledChannels": { "release": { "Version": "1.12.6" } },
                "InstalledVersions": { "1.12.6": { "Path": version_dir.to_string_lossy() } },
            });
            std::fs::write(juliaup_dir.join("juliaup.json"), config.to_string()).unwrap();
            let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
            let _g2 = EnvGuard::capture("HOME");
            let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
            std::env::remove_var("SOT_JULIA_BIN");
            std::env::set_var("HOME", home.path());
            std::env::remove_var("JULIAUP_DEPOT_PATH");
            let err = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap_err();
            assert!(err.contains("app-execution alias"), "{err}");
        }

        #[test]
        fn path_skips_alternate_alias() {
            let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let home = tempfile::tempdir().unwrap();
            let sp = spellings();
            let real_dir = tempfile::tempdir().unwrap();
            let real = real_dir.path().join(JULIA_EXE);
            sot_log::test_exec::write_executable(&real, vec![0u8; 8192]);
            let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
            let _g2 = EnvGuard::capture("HOME");
            let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
            std::env::remove_var("SOT_JULIA_BIN");
            std::env::set_var("HOME", home.path());
            std::env::remove_var("JULIAUP_DEPOT_PATH");
            let path = std::env::join_paths([sp.link_dir.as_path(), real_dir.path()]).unwrap();
            assert_eq!(resolve_bin_on(Some(path.as_os_str())), Ok((real.to_string_lossy().into_owned(), "PATH")));
            let only_alias = std::env::join_paths([sp.link_dir.as_path()]).unwrap();
            assert!(resolve_bin_on(Some(only_alias.as_os_str())).unwrap_err().contains("rejected"));
        }
    }
}
