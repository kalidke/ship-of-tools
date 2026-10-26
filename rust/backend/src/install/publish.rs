//! Installer enrollment of the two known clones. Shell setup owns placement and readiness; platform owns the list
//! location. Discovery is read-only, before install gates, and never searches beyond the original working folder.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sot_platform::host::env::Env;

use super::observe::{self, Answer, Live};
use super::verdict::{Found, Stop};
use super::say;

#[derive(Debug, Clone)]
struct Clone {
    checkout: PathBuf,
    common: PathBuf,
}

#[derive(Clone, Copy)]
struct Bounds {
    git: Duration,
    setup: Duration,
}

// One Git read is much smaller than setup's many Git/Bash processes, especially under parallel Windows tests.
const INSTALL_BOUNDS: Bounds = Bounds { git: Duration::from_secs(60), setup: Duration::from_secs(300) };

pub(super) struct Plan {
    env: Env,
    git: PathBuf,
    bash: PathBuf,
    list: PathBuf,
    caller: Option<Clone>,
    bounds: Bounds,
}

pub(super) struct Enrolled<'a> {
    plan: &'a Plan,
    source: PathBuf,
    commit: String,
    clones: Vec<Clone>,
    base: PathBuf,
}

fn stop(detail: impl Into<String>) -> Stop {
    Stop::at("publish guard", 1, detail)
}

/// A child of the guard within `bound`: its answer at any exit, for the caller to read; with none, the stop.
fn run(command: &mut Command, purpose: &str, bound: Duration) -> Result<Answer, Stop> {
    observe::ask(&Live, command, bound, |a| Ok(a.clone())).or(|why| stop(format!("{purpose}: {why}")))
}

fn scope(found: &mut Found) {
    found.checked("publish guard scope: installed base and recognized caller only; other clones not discovered");
}

fn executable(env: &Env, name: &str) -> Option<PathBuf> {
    std::env::split_paths(env.get("PATH")?).map(|p| p.join(name)).find(|p| p.is_file())
}

/// Derive Bash from the Git installation, never Windows' WSL bash.exe on PATH.
fn interpreter(env: &Env, git: &Path) -> Result<PathBuf, Stop> {
    if !cfg!(windows) {
        return executable(env, "bash").ok_or_else(|| stop("Bash is required for publish guard setup"));
    }
    let git = git.canonicalize().map_err(|_| stop("cannot resolve Git for Windows"))?;
    for parent in git.ancestors().skip(1).take(3) {
        for rel in ["bin/bash.exe", "usr/bin/bash.exe"] {
            let bash = parent.join(rel);
            if bash.is_file() {
                return Ok(bash);
            }
        }
    }
    Err(stop("Git for Windows Bash is required for publish guard setup; install Git for Windows and put its cmd folder on PATH"))
}

/// Bash accepts native drive paths with forward slashes. Canonical Windows paths may carry the extended prefix.
fn bash_path(path: &Path) -> Result<String, Stop> {
    let text = path.to_str().ok_or_else(|| stop("publish guard paths must be UTF-8"))?;
    if cfg!(windows) {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(format!("//{}", unc.replace('\\', "/")));
        }
        return Ok(text.strip_prefix(r"\\?\").unwrap_or(text).replace('\\', "/"));
    }
    Ok(text.to_string())
}

/// Parse only GitHub's ordinary HTTPS and SSH forms into an exact owner/repository pair. In particular a URL that
/// merely contains the repository's name, uses another host, or has extra path components never identifies it.
fn repository(url: &str) -> Option<String> {
    let path = if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = match scheme {
            "https" => authority,
            "ssh" => authority.strip_prefix("git@")?,
            _ => return None,
        };
        if !host.eq_ignore_ascii_case("github.com") { return None; }
        path
    } else {
        url.strip_prefix("git@github.com:")?
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))) {
        return None;
    }
    Some(path.to_ascii_lowercase())
}

fn same_origin(actual: &str, expected: &str) -> bool {
    match (repository(actual), repository(expected)) {
        (Some(a), Some(b)) => a == b,
        // SOT_INSTALL_ORIGIN is the existing local-fixture facility. No filesystem crawl or URL substring match.
        (None, None) => Path::new(actual).canonicalize().ok().zip(Path::new(expected).canonicalize().ok()).is_some_and(|(a, b)| a == b),
        _ => false,
    }
}

impl Plan {
    pub(super) fn capture() -> Result<Self, Stop> {
        let cwd = std::env::current_dir().map_err(|_| stop("cannot read the original working folder"))?;
        let env = Env::process();
        Self::discover(&cwd, &super::origin(&env), env, INSTALL_BOUNDS)
    }

    fn discover(cwd: &Path, origin: &str, mut env: Env, bounds: Bounds) -> Result<Self, Stop> {
        // Git's repository/config selectors cannot redirect discovery or the later setup. Preserve actual global
        // and system config: an effective hooksPath there is the owner's choice, and must remain a conflict.
        for key in ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE", "GIT_IMPLICIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE", "GIT_PREFIX", "GIT_SHALLOW_FILE", "GIT_GRAFT_FILE",
            "GIT_REPLACE_REF_BASE", "GIT_CONFIG", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM"] {
            env = env.without(key);
        }
        env = env.with("LC_ALL", "C").with("GIT_TERMINAL_PROMPT", "0").with("GIT_NO_REPLACE_OBJECTS", "1");
        let git = executable(&env, if cfg!(windows) { "git.exe" } else { "git" }).ok_or_else(|| stop("Git is required for publish guard enrollment"))?;
        let bash = interpreter(&env, &git)?;
        if cfg!(windows) {
            let bin = bash.parent().ok_or_else(|| stop("cannot resolve the Git Bash tools folder"))?;
            let root = bin.parent().ok_or_else(|| stop("cannot resolve the Git Bash installation"))?;
            let mut dirs = vec![PathBuf::from(bash_path(bin)?), PathBuf::from(bash_path(&root.join("usr/bin"))?)];
            if let Some(path) = env.get("PATH") { dirs.extend(std::env::split_paths(path)); }
            let path = std::env::join_paths(dirs).map_err(|_| stop("cannot construct the Git Bash tool path"))?;
            env.set("PATH", path);
        }
        let config = sot_platform::host::state_dir::sot_config_dir_in(&env).ok_or_else(|| stop("cannot resolve the platform config folder"))?;
        if !config.is_absolute() {
            return Err(stop("the platform config folder must be absolute"));
        }
        let mut plan = Self { env, git, bash, list: config.join("publish-list.txt"), caller: None, bounds };
        let top = run(plan.git(cwd).args(["rev-parse", "--show-toplevel"]), "caller discovery", plan.bounds.git)?;
        if top.exit != 0 {
            if top.exit == 128 && top.err.contains("not a git repository") {
                say("publish guard: original working folder is outside Git; no caller clone enrolled");
                return Ok(plan);
            }
            return Err(stop("cannot identify the caller checkout"));
        }
        let checkout = PathBuf::from(top.first_line());
        let url = run(plan.git(&checkout).args(["config", "--local", "--no-includes", "--get", "remote.origin.url"]), "caller origin", plan.bounds.git)?;
        if url.exit == 1 || (url.exit == 0 && !same_origin(&url.first_line(), origin)) {
            say(&format!("publish guard: unrelated caller checkout kept: {}", checkout.display()));
        } else if url.exit != 0 {
            return Err(stop("cannot read the caller clone's origin"));
        } else {
            plan.caller = Some(plan.clone_at(&checkout)?);
        }
        Ok(plan)
    }

    fn git(&self, dir: &Path) -> Command {
        let mut c = Command::new(&self.git);
        c.env_clear().envs(self.env.iter()).args(["-c", "http.lowSpeedLimit=1000", "-c", "http.lowSpeedTime=60", "-C"]).arg(dir);
        c
    }

    fn clone_at(&self, checkout: &Path) -> Result<Clone, Stop> {
        let ran = run(self.git(checkout).args(["rev-parse", "--git-common-dir"]), "common Git directory", self.bounds.git)?;
        let path = PathBuf::from(ran.first_line());
        let path = if path.is_absolute() { path } else { checkout.join(path) };
        let common = if ran.exit == 0 { path.canonicalize().ok() } else { None };
        let common = common.ok_or_else(|| stop(format!("cannot resolve the common Git directory for {}", checkout.display())))?;
        Ok(Clone { checkout: checkout.to_path_buf(), common })
    }

    fn verified_source(&self, source: &Path, commit: &str) -> Result<bool, Stop> {
        let head = run(self.git(source).args(["rev-parse", "HEAD"]), "source HEAD verification", self.bounds.git)?;
        let paths = ["scripts/setup-publish-guard.sh", "scripts/hooks/pre-push", "scripts/hooks/pre-push-shim", "scripts/hooks/publish-guard-lib.sh", "scripts/hooks/publish-guard-scan.awk"];
        Ok(head.exit == 0 && head.first_line() == commit
            && paths.iter().all(|p| std::fs::symlink_metadata(source.join(p)).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()))
            && run(self.git(source).args(["diff", "--quiet", "--no-ext-diff", "--no-textconv", "HEAD", "--"]).args(paths), "source diff verification", self.bounds.git)?.exit == 0)
    }

    fn setup(&self, source: &Path, clone: &Clone, check: bool) -> Result<Answer, Stop> {
        let mut c = Command::new(&self.bash);
        c.env_clear().envs(self.env.iter()).arg(bash_path(&source.join("scripts/setup-publish-guard.sh"))?)
            .arg(bash_path(&clone.checkout)?).arg("--list").arg(bash_path(&self.list)?).arg("--installer");
        if let Some(tmp) = self.env.get_nonempty("TMPDIR") { c.env("TMPDIR", bash_path(Path::new(tmp))?); }
        if check { c.arg("--check-only"); }
        let ran = run(&mut c, if check { "read-only publish guard check" } else { "publish guard enrollment" }, self.bounds.setup)?;
        for line in ran.out.lines().chain(ran.err.lines()) { say(line); }
        Ok(ran)
    }

    pub(super) fn enroll<'a>(&'a self, source: &Path, commit: &str, base: &Path, found: &mut Found) -> Result<Enrolled<'a>, Stop> {
        super::verdict::step("publish guard");
        if !self.verified_source(source, commit)? {
            return Err(stop("the target checkout has no verified publish guard setup"));
        }
        let mut clones = vec![self.clone_at(base)?];
        if let Some(caller) = &self.caller {
            if !clones.iter().any(|c| c.common == caller.common) { clones.push(caller.clone()); }
        }
        let mut placed = Vec::new();
        scope(found);
        for clone in clones {
            if self.clone_at(&clone.checkout)?.common != clone.common {
                return Err(stop("a clone's common Git directory changed since discovery"));
            }
            match self.setup(source, &clone, false)?.exit {
                0 | 3 => placed.push(clone),
                4 => {
                    self.note(found, base, &clone, "preserved conflict");
                    found.step(format!("resolve the preserved publish hook conflict in {} (see setup diagnostics above), then run this installer again", clone.checkout.display()));
                }
                _ => return Err(stop(format!("enrollment failed for {} (see setup diagnostics above)", clone.checkout.display()))),
            }
        }
        say("publish guard: only the installed base and recognized caller clones were considered; other clones were not discovered");
        Ok(Enrolled { plan: self, source: source.to_path_buf(), commit: commit.to_string(), clones: placed, base: base.to_path_buf() })
    }

    fn note(&self, found: &mut Found, base: &Path, clone: &Clone, state: &str) {
        let role = if clone.checkout == base { "base" } else { "caller" };
        found.checked(format!("publish guard checked {role} {}: {state}", clone.checkout.display()));
        if let Some(caller) = &self.caller {
            if clone.checkout == base && caller.common == clone.common {
                found.checked(format!("publish guard checked caller {}: {state} (shares base)", caller.checkout.display()));
            }
        }
    }

    /// Windows may have only armed a target whose setup is not yet available. Never report it checked or applied.
    #[cfg_attr(unix, allow(dead_code, reason = "Windows pending-apply path"))]
    pub(super) fn pending(&self, source: &Path, commit: &str, base: &Path, found: &mut Found) -> Result<(), Stop> {
        super::verdict::step("publish guard");
        if source.join("scripts/setup-publish-guard.sh").is_file() {
            self.enroll(source, commit, base, found)?.check(found);
        } else {
            let caller = self.caller.as_ref().map_or(String::new(), |c| format!(" and caller {}", c.checkout.display()));
            found.step(format!("after applying the update, run this installer again to enroll and check publish guards for base {}{caller}; enrollment is pending", base.display()));
            scope(found);
        }
        Ok(())
    }
}

impl Enrolled<'_> {
    pub(super) fn check(&self, found: &mut Found) {
        super::verdict::step("publish guard check");
        match self.plan.verified_source(&self.source, &self.commit) {
            Ok(true) => {}
            Ok(false) => { found.fail("publish guard", "the setup source changed during the install"); return; }
            Err(error) => { found.fail("publish guard", error.detail); return; }
        }
        for clone in &self.clones {
            // Check the common identity again too; never repair a changed/deleted hook during the final check.
            let now = match self.plan.clone_at(&clone.checkout) {
                Ok(now) => now,
                Err(error) => { found.fail("publish guard", error.detail); return; }
            };
            if now.common != clone.common { found.fail("publish guard", "a clone's common Git directory changed during the install"); return; }
            let code = match self.plan.setup(&self.source, clone, true) {
                Ok(ran) => ran.exit,
                Err(error) => { found.fail("publish guard", error.detail); return; }
            };
            match code {
                0 => {
                    self.plan.note(found, &self.base, clone, "ready");
                    say(&format!("publish guard: ready for {}", clone.checkout.display()));
                }
                3 => {
                    self.plan.note(found, &self.base, clone, "guard installed; policy needs provisioning");
                    found.step(format!("provision a valid private publish-list.txt in the platform config folder, then run this installer again (guard installed for {}; pushes refuse until ready)", clone.checkout.display()));
                }
                _ => { found.fail("publish guard", format!("enrolled state changed or could not be checked for {}", clone.checkout.display())); return; }
            }
        }
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
