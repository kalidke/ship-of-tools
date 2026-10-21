//! `sotd install`: the body of the installers, one implementation for every system (scripts/CLAUDE.md, "Installers").
//! The bootstrap (`scripts/install.sh`, `scripts/install.ps1`) fetched the pack, checked every file of it against
//! `SHA256SUMS`, unpacked it and runs this, the pack's own `sotd`, with `--pack <folder>` before the person's arguments.
//! A run reads the options and the pack, decides from read-only facts (the direction, the role and its gates, the
//! daemon's live work), then lays the release out, wires it, records it, checks what it left, and ends with the one
//! verdict line. It runs the other subsystems' commands (`sotd update`, `same-version`, `topology`, `trust`) as programs.

mod cmd;
mod daemon;
mod julia;
mod options;
mod pack;
mod procs;
mod record;
#[cfg(unix)]
mod role;
#[cfg(unix)]
mod unix;
mod verdict;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

pub(crate) const USAGE: &str = "\
Usage: sotd install --pack <folder> [options]

  The body of scripts/install.sh (Linux, macOS) and scripts/install.ps1 (Windows), which fetch a release or copy an
  --archive folder, check every file against its SHA256SUMS, unpack it into <folder> and run the pack's own sotd here;
  run one of them, not this. It installs the pack into the prefix, or updates or repairs the install there, and ends
  with one line: `PASS: Ship of Tools <v> at <commit> on this computer`, `ONE STEP LEFT: <what a person does>` (exit 0)
  or `FAIL: <check>: <detail>` (exit 1, or 2 for a refusal).

  Unix options: --local | --backend <ssh alias> | --be-only (the role when hosts.toml names no entry for this host),
  --version vX.Y.Z | --archive <folder> (name the target, and pin the install there), --prefix <folder>,
  --hub <ssh alias> (fetch hosts.toml from the hub), --no-service, --force-role-change, --restart-daemon.
  Windows options: -Version vX.Y.Z | -Archive <folder>, -Hub <ssh alias>, -Prefix <folder>, -RestartDaemon.";

/// `sotd install <args>`: the exit code, after the verdict line.
pub(crate) fn run(args: &[String]) -> i32 {
    verdict::watch_signals();
    verdict::finish(body(args))
}

fn body(args: &[String]) -> Result<(verdict::Found, String), verdict::Stop> {
    verdict::step("options");
    let opts = options::parse(args)?;
    #[cfg(unix)]
    return unix::install(opts);
    #[cfg(windows)]
    return windows::install(opts);
}

/// A progress line.
#[cfg_attr(windows, allow(dead_code, reason = "Windows prints its lines as install.ps1 did, with no lead"))]
pub(super) fn say(text: &str) {
    println!("== {text}");
}

/// The person's home folder: `HOME` on Unix, `USERPROFILE` on Windows.
pub(super) fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// An absolute spelling of `p`, from the working folder for a relative one, with no trailing separator.
pub(super) fn absolute(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy();
    let trimmed = s.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') { abs } else { PathBuf::from(trimmed) }
}

/// The version an install at `prefix` runs: its record's, else its installed `sotd`'s own (a record cut inside its
/// version must not hide it from the channel or the never-backwards rule). Empty when neither says.
pub(super) fn installed_version(prefix: &Path) -> String {
    if let Some(v) = record::string(&record::read(&prefix.join("install.json")), "version").filter(|v| !v.is_empty()) {
        return v;
    }
    let sotd = prefix.join("bin").join(pack::exe_name("sotd"));
    if !sotd.is_file() {
        return String::new();
    }
    let ran = cmd::output(std::process::Command::new(&sotd).arg("--version"));
    ran.first_line().split_whitespace().nth(1).unwrap_or("").to_string()
}

/// Which way `sotd update direction` of the pack says the install at `prefix` moves: `newer`, `same`, `older`, or
/// `unknown`. `installed` stands in only where the record names no version.
pub(super) fn direction(sotd: &Path, version: &str, prefix: &Path, installed: &str) -> String {
    let mut c = std::process::Command::new(sotd);
    c.args(["update", "direction", version, "--prefix"]).arg(prefix);
    let recorded = record::string(&record::read(&prefix.join("install.json")), "version").is_some_and(|v| !v.is_empty());
    if !recorded && !installed.is_empty() {
        c.args(["--installed", installed]);
    }
    let ran = cmd::output(&mut c);
    match ran.first_line().as_str() {
        w @ ("newer" | "same" | "older") if ran.ok() => w.to_string(),
        _ => "unknown".to_string(),
    }
}

/// What a build of the line (a `+` in its version) is told about the release a run found, by ancestry and not version
/// text: its commit (the record's, else the `+<sha>` of its version) against the pack's `COMMIT`, in the install's own
/// base clone. `forward`, `older` or `unknown: <why>`.
pub(super) fn build_direction(prefix: &Path, installed: &str, release_commit: &str) -> String {
    let base = prefix.join("repo").join("base");
    let recorded = record::string(&record::read(&prefix.join("install.json")), "commit").unwrap_or_default();
    let built = if is_commit(&recorded) {
        recorded
    } else {
        installed.split_once('+').map_or("", |(_, b)| b).trim_end_matches("-dirty").to_string()
    };
    if built.is_empty() || built == "src" {
        return "unknown: this build names no commit".into();
    }
    if !base.join(".git").exists() && !base.join("HEAD").is_file() {
        return format!("unknown: no base clone at {}", base.display());
    }
    let git = |args: &[&str]| cmd::output(cmd::git_in(&base).args(args)).ok();
    if !git(&["cat-file", "-e", &format!("{release_commit}^{{commit}}")]) && !git(&["fetch", "--quiet", "origin", release_commit]) {
        return "unknown: could not fetch the commit of the release".into();
    }
    if !git(&["rev-parse", "--verify", "--quiet", &format!("{built}^{{commit}}")]) {
        return format!("unknown: this build's commit {} is not in {}", &built[..built.len().min(9)], base.display());
    }
    if git(&["merge-base", "--is-ancestor", &built, release_commit]) {
        "forward".into()
    } else if git(&["merge-base", "--is-ancestor", release_commit, &built]) {
        "older".into()
    } else {
        "unknown: neither the release nor this build contains the other".into()
    }
}

/// The stop of a run that names no target over a build of the line: it says which way the release found moves it.
pub(super) fn build_stop(installed: &str, tag: &str, way: &str, archive_flag: &str, version_flag: &str) -> String {
    let advice = match way {
        "forward" => format!("the newest release, {tag}, contains this build: naming it with {version_flag} moves this computer forward"),
        "older" => format!("the newest release, {tag}, does not contain this build: naming it with {version_flag} moves this computer back"),
        other => format!("could not tell which way the newest release, {tag}, moves this computer ({})", other.trim_start_matches("unknown: ")),
    };
    format!("this computer runs {installed}, a build of the line, not a release: install a pack of the line with {archive_flag} <folder>; {advice}; ask the person who runs it which they want")
}

pub(super) fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The variables the run reads that the bootstrap sets: the tag its prelude resolved, and whether the person named it.
pub(super) fn implicit_run() -> bool {
    std::env::var("SOT_INSTALL_IMPLICIT").as_deref() == Ok("1")
}

pub(super) fn bootstrap_tag() -> Option<String> {
    std::env::var("SOT_INSTALL_TAG").ok().filter(|t| !t.is_empty())
}

/// The release repository (`owner/name`): `SOT_INSTALL_REPO`, else the project's.
pub(super) fn install_repo() -> String {
    std::env::var("SOT_INSTALL_REPO").ok().filter(|r| !r.trim().is_empty()).unwrap_or_else(|| sot_updater::identity::DEFAULT_REPO.to_string())
}

/// The URL the base clone is cloned from: `SOT_INSTALL_ORIGIN` (a test's local origin), else the repository's.
pub(super) fn origin() -> String {
    std::env::var("SOT_INSTALL_ORIGIN").ok().filter(|o| !o.is_empty()).unwrap_or_else(|| format!("https://github.com/{}", install_repo()))
}

/// A bound in seconds from the environment, else `default`.
pub(super) fn secs_env(name: &str, default: u64) -> std::time::Duration {
    std::time::Duration::from_secs(std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default))
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
