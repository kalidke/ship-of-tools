//! The Julia an install leaves (scripts/CLAUDE.md, Installers: "The Julia the daemon will run meets the kernel's
//! floor"). The floor is `[compat] julia` of `julia/kernel/Project.toml`. The daemon's Julia is the one
//! `sidecars/julia.rs` resolves: `SOT_JULIA_BIN`, else juliaup's default channel read from `juliaup.json` (no Julia
//! starts, so no startup file runs; its Version string), else the PATH julia; the comm refresh runs the PATH julia.
//! Julia's steps run with `TMPDIR` in the run's scratch folder, so a download Pkg leaves when a person stops the run
//! goes with it.

use std::path::{Path, PathBuf};
use std::process::Command;

use sot_platform::host::env::Env;

use super::observe::{self, Fact, Machine, Unknown, Why};
use super::{cmd, verdict::Found};

/// The channel a Unix install adds when no Julia at or above it is on PATH, and selects for its steps when juliaup has it.
#[cfg_attr(windows, allow(dead_code, reason = "Windows installs no Julia"))]
pub(super) const CHANNEL: &str = "1.12";

/// The first `major.minor` of the kernel's `[compat] julia`, whatever its spelling (`1.12.4`, `~1.12`, `^1.12, 1.13`).
pub(super) fn floor(checkout: &Path) -> Option<String> {
    let text = std::fs::read_to_string(checkout.join("julia/kernel/Project.toml")).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let spec = table.get("compat")?.get("julia")?.as_str()?.to_string();
    first_major_minor(&spec)
}

fn first_major_minor(s: &str) -> Option<String> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let major: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let rest = rest[major.len()..].strip_prefix('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!minor.is_empty()).then(|| format!("{major}.{minor}"))
}

/// Whether `version`'s major.minor is at or above `floor`'s; a version that does not start `N.N` is not.
pub(super) fn at_floor(version: &str, floor: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32)> {
        let mm = first_major_minor(v.split('+').next().unwrap_or(""))?;
        if !v.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let (a, b) = mm.split_once('.')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    matches!((parse(version), parse(floor)), (Some(v), Some(f)) if v >= f)
}

/// juliaup's state file: `$JULIAUP_DEPOT_PATH/juliaup/juliaup.json`, else `~/.julia/juliaup/juliaup.json`.
fn juliaup_json(env: &Env) -> Option<serde_json::Value> {
    let root = match env.get_nonempty("JULIAUP_DEPOT_PATH") {
        Some(d) => PathBuf::from(d).join("juliaup"),
        None => super::home_dir(env)?.join(".julia").join("juliaup"),
    };
    serde_json::from_str(&std::fs::read_to_string(root.join("juliaup.json")).ok()?).ok()
}

/// The version of juliaup's default channel, its build metadata cut; `None` without a readable default.
pub(super) fn default_version(json: &serde_json::Value) -> Option<String> {
    let default = json.get("Default")?.as_str()?;
    let v = json.get("InstalledChannels")?.get(default)?.get("Version")?.as_str()?;
    Some(v.split('+').next().unwrap_or(v).to_string()).filter(|v| !v.is_empty())
}

/// `julia`'s own version, asked with no startup file.
pub(super) fn version_of(m: &dyn Machine, julia: &str, dir: Option<&Path>) -> Fact<String> {
    let mut c = Command::new(julia);
    c.args(["--startup-file=no", "-e", "print(VERSION)"]);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    observe::ask(m, &mut c, observe::ASK_BOUND, |a| Some(a.zero()?.out.trim().to_string()).filter(|v| !v.is_empty()).ok_or_else(|| Why::Malformed("no version".into())))
}

/// Where the daemon's Julia comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    /// An absolute `SOT_JULIA_BIN`, with the version it gave.
    Bin(Fact<String>),
    /// A `SOT_JULIA_BIN` that is not absolute, which the daemon refuses.
    Relative(String),
    /// juliaup's default channel, at this version.
    Up(String),
    /// The PATH julia.
    Path,
}

/// What the floor check reads.
#[derive(Debug, Clone)]
pub(super) struct Facts {
    pub floor: String,
    /// The PATH julia's version: the comm refresh's, and the daemon's when its source is `Path`.
    pub refresh: Fact<String>,
    pub source: Source,
    pub juliaup_on_path: bool,
    /// juliaup lists the floor's channel, or the run selected a channel for its steps.
    pub floor_channel: bool,
    /// The run's steps run on a selected channel, not the PATH julia.
    pub selected: bool,
    /// The PATH julia's path, for "ahead of" it.
    pub path_julia: String,
}

/// The check's answer: nothing is left, a step (the install is whole), or a stop before a Julia step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Judged {
    Ok,
    Step(String),
    Stop(String),
}

/// Whether `v` is at the floor, and how it is said: `<who> runs Julia <v>`, or why its version could not be asked.
fn said(who: &str, v: Fact<&String>, floor: &str) -> (bool, String) {
    v.read(|v| (at_floor(v, floor), format!("{who} runs Julia {v}")), |why| (false, format!("the Julia {who} runs could not be asked for its version ({why})")))
}

/// The one line, and the Julia it is about.
pub(super) fn judge(f: &Facts) -> Judged {
    let up;
    let daemon = match &f.source {
        Source::Bin(v) => Some(v.as_ref()),
        Source::Up(v) => {
            up = Fact::known(v.clone());
            Some(up.as_ref())
        }
        Source::Path => Some(f.refresh.as_ref()),
        Source::Relative(_) => None,
    };
    let (daemon_ok, daemon_said) = daemon.map_or((false, String::new()), |v| said("the daemon", v, &f.floor));
    let daemon_low = !daemon_ok;
    let mut bad = match &f.source {
        Source::Relative(bin) => format!("SOT_JULIA_BIN={bin} is not an absolute path, which the daemon refuses"),
        _ if daemon_low => daemon_said,
        _ => String::new(),
    };
    let (refresh_ok, refresh_said) = said("the comm refresh", f.refresh.as_ref(), &f.floor);
    if f.source != Source::Path && !refresh_ok {
        bad = if bad.is_empty() { refresh_said } else { format!("{bad} and {refresh_said}") };
    }
    if bad.is_empty() {
        return Judged::Ok;
    }
    let floor = &f.floor;
    let fix = match &f.source {
        Source::Relative(_) => "make SOT_JULIA_BIN an absolute path or unset it".to_string(),
        Source::Bin(_) if daemon_low => format!("point SOT_JULIA_BIN at Julia {floor} or later, or unset it"),
        Source::Up(_) if daemon_low && f.juliaup_on_path => {
            let default = format!("juliaup default {floor}");
            if f.floor_channel {
                default
            } else if cfg!(windows) {
                format!("juliaup add {floor}; {default}")
            } else {
                format!("juliaup add {floor} && {default}")
            }
        }
        Source::Path => format!("install Julia {floor} or later and put it first on PATH"),
        Source::Up(_) if daemon_low => format!("install Julia {floor} or later and put it first on PATH"),
        _ => format!("put a Julia {floor} or later ahead of {} on PATH", if f.path_julia.is_empty() { "julia" } else { &f.path_julia }),
    };
    let line = format!("{bad}, below the {floor} the kernel needs: run  {fix}");
    // Steps that run on the PATH julia cannot pass below the floor, so the run ends before one runs.
    if !f.selected && f.refresh.as_ref().read(|v| !at_floor(v, floor), |_| false) {
        return Judged::Stop(line);
    }
    Judged::Step(line)
}

/// Read the facts for the checkout at `checkout`. `selected` is the channel the run's steps take, if any.
pub(super) fn facts(m: &dyn Machine, checkout: &Path, selected: Option<&str>) -> Result<Facts, String> {
    let floor = floor(checkout).ok_or_else(|| format!("{} names no Julia compat", checkout.join("julia/kernel/Project.toml").display()))?;
    let path_julia = observe::which(m, "julia").map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let refresh = if path_julia.is_empty() { Fact::unknown(Unknown::new("julia", Why::NotStarted("no julia on PATH".into()))) } else { version_of(m, "julia", Some(checkout)) };
    let env = m.env();
    let json = juliaup_json(&env);
    let bin = env.text("SOT_JULIA_BIN").trim().to_string();
    let source = if !bin.is_empty() {
        if Path::new(&bin).is_absolute() { Source::Bin(version_of(m, &bin, None)) } else { Source::Relative(bin) }
    } else if let Some(v) = json.as_ref().and_then(default_version) {
        Source::Up(v)
    } else {
        Source::Path
    };
    let listed = json.as_ref().and_then(|j| j.get("InstalledChannels")?.get(&floor)).is_some();
    Ok(Facts {
        floor,
        refresh,
        source,
        juliaup_on_path: observe::on_path(m, "juliaup"),
        floor_channel: selected.is_some() || listed,
        selected: selected.is_some(),
        path_julia,
    })
}

/// A Julia step: `julia [+channel] <args>`, with `TMPDIR` (and Windows' `TEMP` and `TMP`) in `scratch`, shown as it runs;
/// whether it succeeded.
pub(super) fn step(m: &dyn Machine, selected: Option<&str>, scratch: &Path, args: &[&str]) -> bool {
    let mut c = Command::new("julia");
    if let Some(chan) = selected {
        c.arg(format!("+{chan}"));
    }
    c.args(args).env("TMPDIR", scratch);
    if cfg!(windows) {
        c.env("TEMP", scratch).env("TMP", scratch);
    }
    m.show(&mut c) == Some(0)
}

/// The environments a daemon's checkout instantiates, with the code each runs.
pub(super) const ENVS: [(&str, &str); 3] = [
    ("kernel", "using Pkg; Pkg.instantiate()"),
    ("repl", "using Pkg; Pkg.instantiate()"),
    ("pluto", "using Pkg; Pkg.instantiate(); Pkg.precompile(); using Pluto"),
];

/// Instantiate the checkout's Julia environments, a stale untracked `Manifest.toml` dropped first (one that predates a
/// dependency the tag added fails instantiate; a manifest the tag tracks is never touched, which would dirty the
/// read-only checkout). A failure is the run's FAIL; a manifest kept because git could not say is named in the verdict.
pub(super) fn instantiate(m: &dyn Machine, checkout: &Path, selected: Option<&str>, scratch: &Path, repair: bool, say: &dyn Fn(&str), found: &mut Found) {
    for (env, manifest) in stale_manifests(m, checkout, repair, found) {
        say(&format!("dropping stale julia/{env}/Manifest.toml (fresh resolve at this tag)"));
        let _ = std::fs::remove_file(&manifest);
    }
    say("instantiating julia envs (first run takes a few minutes)");
    for (env, code) in ENVS {
        let project = format!("--project={}", checkout.join("julia").join(env).display());
        if !step(m, selected, scratch, &[&project, "-e", code]) {
            found.fail("julia", format!("instantiating julia/{env} failed; run it again"));
        }
    }
}

/// Whether git says the environment's manifest is not tracked in `checkout` (`ls-files --error-unmatch` exits 1; 0 is
/// tracked).
fn untracked(m: &dyn Machine, checkout: &Path, env: &str) -> Fact<bool> {
    observe::ask(m, cmd::git_in(checkout).args(["ls-files", "--error-unmatch", &format!("julia/{env}/Manifest.toml")]), observe::ASK_BOUND, |a| match a.exit {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(a.failed()),
    })
}

/// The untracked manifests a run drops before it instantiates, so a manifest that predates a dependency the tag added
/// cannot fail it. A repair run keeps them: they are this checkout's own resolution, and a repair moves nothing. Only an
/// answer that says untracked drops one; a git that cannot say keeps it, and the verdict says so.
pub(super) fn stale_manifests(m: &dyn Machine, checkout: &Path, repair: bool, found: &mut Found) -> Vec<(&'static str, PathBuf)> {
    if repair {
        return Vec::new();
    }
    let mut stale = Vec::new();
    for (env, _) in ENVS {
        let manifest = checkout.join("julia").join(env).join("Manifest.toml");
        if !manifest.is_file() {
            continue;
        }
        untracked(m, checkout, env).read(
            |untracked| if untracked { stale.push((env, manifest.clone())) },
            |why| found.checked(format!("kept julia/{env}/Manifest.toml: git could not say whether the checkout tracks it ({why})")),
        );
    }
    stale
}

/// Bring the comm install to the checkout: `ShipTools.update_comm()` from the checkout's project.
pub(super) fn update_comm(m: &dyn Machine, checkout: &Path, selected: Option<&str>, scratch: &Path) -> bool {
    let project = format!("--project={}", checkout.display());
    step(m, selected, scratch, &[&project, "-e", "using ShipTools; ShipTools.update_comm()"])
}

#[cfg(test)]
#[path = "julia_tests.rs"]
mod tests;
