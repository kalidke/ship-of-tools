// julia.rs — resolve the real `julia` binary the daemon spawns, shared by
// kernel.rs, repl.rs, and pluto.rs (each keeps its own supervisor; only
// resolution is shared — one function, no privileged caller).
//
// Invariant: never spawn a PATH candidate this resolver has not verified is
// plausibly real Julia. An explicit `SOT_JULIA_BIN` is exempt (an operator's
// own choice, format-validated but not second-guessed).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Resolve the `julia` binary honestly. Order:
/// 1. `SOT_JULIA_BIN`, trimmed — must be an absolute path (upstream juliaup's
///    own override semantics); a non-empty but relative value is a hard
///    error rather than a silently-ignored fall-through, so a typo in an
///    explicit override is never masked by auto-detection picking something
///    else.
/// 2. juliaup's DEFAULT-CHANNEL binary, resolved by reading `juliaup.json`
///    directly (bypasses the launcher shim) — re-resolved on every call, so
///    a channel update or a removed version recovers on the very next spawn
///    attempt rather than replaying a permanently cached path.
/// 3. A PATH candidate, rejected (and the search continued to the next
///    entry) if it is not a real executable (Unix: the executable bit is
///    unset) or is a Windows Store app-execution alias (a `WindowsApps`
///    path component). If every candidate found is rejected, this is an
///    error — bare `"julia"` would just re-resolve the same rejected file,
///    so it is never offered as a fallback in that case. An EMPTY PATH
///    search (no candidate file exists at all) still returns bare
///    `"julia"`: there is nothing to have rejected, and the OS's own
///    lookup might succeed via a mechanism this search didn't enumerate.
pub(crate) fn resolve_bin() -> Result<(String, &'static str), String> {
    if let Some(v) = std::env::var_os("SOT_JULIA_BIN") {
        let trimmed = v.to_string_lossy().trim().to_string();
        if !trimmed.is_empty() {
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
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    let candidates = candidates_on_path(exe_name);
    if candidates.is_empty() {
        return Ok(("julia".to_string(), "PATH (unverified fallback)"));
    }
    let mut first_rejection = None;
    for candidate in candidates {
        match looks_like_fake_julia(&candidate) {
            None => return Ok((candidate.to_string_lossy().into_owned(), "PATH")),
            Some(reason) => {
                tracing::warn!(
                    path = %candidate.display(),
                    reason,
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

/// For callers that don't need the resolution failure reason (`repl.rs`,
/// `pluto.rs` — simpler supervisors than the kernel's, unchanged by this
/// module beyond sharing this resolver) and want the OLD "just give me a
/// string" ergonomics: falls back to bare `"julia"` on any resolution
/// failure, logging why. The kernel supervisor calls `resolve_bin` directly
/// instead, since its failure becomes a typed `Dead` reason, not a
/// swallowed warning.
pub(crate) fn resolve_bin_or_bare() -> String {
    match resolve_bin() {
        Ok((path, _source)) => path,
        Err(reason) => {
            tracing::warn!(reason, "julia resolution failed; falling back to bare `julia`");
            "julia".to_string()
        }
    }
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
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    let bin = juliaup_dir.join(rel_path).join("bin").join(exe_name);
    if bin.is_file() {
        Ok(bin)
    } else {
        Err(format!("resolved binary missing on disk: {}", bin.display()))
    }
}

/// True if `path`'s string form has a path component exactly equal to
/// "WindowsApps" (case-insensitive), split on `\` — a STRING predicate
/// rather than `std::path::Path::components()`, which parses by the HOST
/// platform's separator conventions and would not split a Windows-shaped
/// path apart when run on Unix. Kept separate from `looks_like_fake_julia`
/// (which is only ever exercised for real under `cfg(windows)`) so the rule
/// itself is unit-tested on every platform, not just compiled for one.
fn is_windows_apps_alias_path(path: &Path) -> bool {
    path.to_string_lossy()
        .split('\\')
        .any(|seg| seg.eq_ignore_ascii_case("WindowsApps"))
}

/// A candidate `julia`/`julia.exe` found on PATH that must be rejected
/// before the daemon spawns it, with the reason:
/// - Unix: not executable (the field failure's shape under a different
///   name — any placeholder lacking the exec bit).
/// - Windows: a Store app-execution alias — PATH commonly resolves
///   `julia.exe` to `...\WindowsApps\julia.exe` for a user who has never
///   run `julia` interactively (the Store's placeholder for "you don't
///   have this app, want to install it?"). It reports as a normal file but
///   is a reparse-point stub; spawned by a detached daemon (no interactive
///   shell to field the Store prompt) it exits at once with no output —
///   the exact field failure.
#[cfg(unix)]
fn looks_like_fake_julia(path: &Path) -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) if md.permissions().mode() & 0o111 == 0 => Some("not executable"),
        Ok(_) => None,
        Err(_) => Some("metadata unreadable"),
    }
}

#[cfg(windows)]
fn looks_like_fake_julia(path: &Path) -> Option<&'static str> {
    if is_windows_apps_alias_path(path) {
        Some("path has a \\WindowsApps\\ component (Windows Store app-execution alias)")
    } else {
        None
    }
}

/// Every `julia`/`julia.exe` reachable via `PATH`, in PATH order, without
/// relying on the OS's own single-candidate lookup (`Command::new` spawning
/// straight off PATH can't skip a rejected first match and try the next
/// entry — this can).
fn candidates_on_path(exe_name: &str) -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|dir| dir.join(exe_name))
                .filter(|c| c.is_file())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ENV_TEST_LOCK;

    // --- is_windows_apps_alias_path (portable path predicate) -------------

    #[test]
    fn windows_apps_alias_path_matches_regardless_of_case() {
        assert!(is_windows_apps_alias_path(Path::new(
            r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\julia.exe"
        )));
        assert!(is_windows_apps_alias_path(Path::new(
            r"c:\users\x\appdata\local\microsoft\windowsapps\julia.exe"
        )));
    }

    #[test]
    fn windows_apps_alias_path_requires_an_exact_component_not_a_substring() {
        assert!(!is_windows_apps_alias_path(Path::new(
            r"C:\Users\x\MyWindowsAppsBackup\julia.exe"
        )));
    }

    #[test]
    fn a_large_real_looking_path_is_not_an_alias() {
        // Proves the OLD portable size rule is gone: a large file at a
        // plain path is not flagged by the (now path-only) predicate.
        assert!(!is_windows_apps_alias_path(Path::new(
            r"C:\Users\x\.juliaup\bin\julia.exe"
        )));
    }

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
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("julia");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        assert_eq!(looks_like_fake_julia(&path), None);
    }

    // --- resolve_juliaup_julia_from_config (table test) --------------------

    fn write_fake_juliaup_home(juliaup_dir: &Path, version_dir_name: &str) {
        let bin_dir = juliaup_dir.join(version_dir_name).join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
        std::fs::write(bin_dir.join(exe_name), vec![0u8; 8192]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(bin_dir.join(exe_name)).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(bin_dir.join(exe_name), perms).unwrap();
        }
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

    struct EnvGuard(&'static str, Option<std::ffi::OsString>);
    impl EnvGuard {
        fn capture(key: &'static str) -> Self {
            Self(key, std::env::var_os(key))
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.1.take() {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

    #[test]
    fn sot_julia_bin_wins_outright_when_absolute() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        std::env::set_var("SOT_JULIA_BIN", "  /explicit/override/julia  ");
        std::env::set_var("HOME", "/should/not/matter");
        let (bin, source) = resolve_bin().unwrap();
        assert_eq!(bin, "/explicit/override/julia"); // trimmed
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
        std::fs::write(&real_julia, vec![0u8; 8192]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&real_julia).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&real_julia, perms).unwrap();
        }

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        let _g4 = EnvGuard::capture("PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        std::env::set_var("PATH", path_dir.path());

        let (bin, source) = resolve_bin().unwrap();
        assert_eq!(source, "PATH");
        assert_eq!(bin, real_julia.to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn path_search_skips_a_non_executable_candidate_for_a_real_one_further_along() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let fake_dir = tempfile::tempdir().unwrap();
        let real_dir = tempfile::tempdir().unwrap();
        std::fs::write(fake_dir.path().join("julia"), b"not executable").unwrap();
        let real_julia = real_dir.path().join("julia");
        std::fs::write(&real_julia, vec![0u8; 8192]).unwrap();
        let mut perms = std::fs::metadata(&real_julia).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&real_julia, perms).unwrap();

        let joined_path = std::env::join_paths([fake_dir.path(), real_dir.path()]).unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        let _g4 = EnvGuard::capture("PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        std::env::set_var("PATH", joined_path);

        let (bin, source) = resolve_bin().unwrap();
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
        let _g4 = EnvGuard::capture("PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        std::env::set_var("PATH", path_dir.path());

        let err = resolve_bin().unwrap_err();
        assert!(err.contains("rejected"), "unexpected message: {err}");
    }

    #[test]
    fn falls_back_to_bare_julia_only_when_path_search_found_nothing_at_all() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let empty_path_dir = tempfile::tempdir().unwrap();

        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        let _g4 = EnvGuard::capture("PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        std::env::set_var("PATH", empty_path_dir.path());

        let (bin, _source) = resolve_bin().unwrap();
        assert_eq!(bin, "julia");
    }
}
