//! `sotd install`: the body of the installers, one implementation for every system (scripts/CLAUDE.md, "Installers").
//! The bootstrap (`scripts/install.sh`, `scripts/install.ps1`) fetched the pack, checked every file of it against
//! `SHA256SUMS`, unpacked it and runs this, the pack's own `sotd`, with `--pack <folder>` before the person's arguments.
//! A run reads the options and the pack, decides from read-only facts (the direction, the role and its gates, the
//! daemon's live work), then lays the release out, wires it, records it, checks what it left, and ends with the one
//! verdict line. It runs the other subsystems' commands (`sotd update`, `same-version`, `topology`, `trust`) as programs.

// A question's answer (`observe::Fact`) is never dropped unread: not as a statement (`unused_must_use`), not by
// `let _ =` (clippy, which the gate runs; a deliberate `let _ = fact` fails it).
#![deny(unused_must_use, clippy::let_underscore_must_use)]

mod cmd;
mod daemon;
mod julia;
mod observe;
mod options;
mod pack;
mod procs;
mod publish;
mod record;
#[cfg(unix)]
mod role;
mod supervisor;
#[cfg(unix)]
mod unix;
mod verdict;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};
use std::process::Command;

use observe::{Fact, Machine, Why};
use sot_platform::host::env::Env;

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
    if !cmd::STEPS.is_stopping() {
        println!("== {text}");
    }
}

/// The person's home folder: `HOME` on Unix, `USERPROFILE` on Windows.
pub(super) fn home_dir(env: &Env) -> Option<PathBuf> {
    env.get_nonempty(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// An absolute spelling of `p`, from the working folder for a relative one, with no trailing separator.
pub(super) fn absolute(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy();
    let trimmed = s.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') { abs } else { PathBuf::from(trimmed) }
}

/// The version an install at `prefix` runs: its record's, else its installed `sotd`'s own (a record cut inside its
/// version must not hide it from the channel or the never-backwards rule), empty when there is no install to read. An
/// installed `sotd` whose `--version` gives no version is unknown: an install that is there and unreadable is never
/// taken for none.
fn installed_version(m: &dyn Machine, prefix: &Path) -> Fact<String> {
    if let Some(v) = record::string(&record::read(&prefix.join("install.json")), "version").filter(|v| !v.is_empty()) {
        return Fact::known(v);
    }
    let sotd = prefix.join("bin").join(pack::exe_name("sotd"));
    if !sotd.is_file() {
        return Fact::known(String::new());
    }
    observe::ask(m, Command::new(&sotd).arg("--version"), observe::ASK_BOUND, |a| {
        let version = a.zero()?.first_line().split_whitespace().nth(1).unwrap_or("").to_string();
        if version.is_empty() { Err(Why::Malformed(format!("no version ('{}')", a.first_line()))) } else { Ok(version) }
    })
}

/// The installed version a run goes on with. An install that is there and unreadable stops a run that names no target
/// (it never moves an install it cannot compare, `flag` being this system's spelling of `--version`); a run that names its
/// target moves either way, so it goes on with no version and its verdict says why.
fn installed_for(read: Fact<String>, prefix: &Path, implicit: bool, flag: &str, found: &mut verdict::Found) -> Result<String, verdict::Stop> {
    read.read(Ok, |why| {
        if implicit {
            return Err(verdict::Stop::at("direction", 2, format!("the install at {} has a sotd whose version cannot be read ({why}); a run that names no target never moves it unasked: name the version you want with {flag} vX.Y.Z", prefix.display())));
        }
        found.checked(format!("the version installed at {} before this run could not be read ({why}); the named target was installed over it", prefix.display()));
        Ok(String::new())
    })
}

/// The daemon's endpoint name an installed `sotd` prints for `session-socket-path <role>`: its first line when it
/// exits 0 and `valid` accepts it.
fn ask_endpoint(m: &dyn Machine, sotd: &Path, role: &str, valid: impl Fn(&str) -> bool) -> Fact<String> {
    observe::ask(m, Command::new(sotd).args(["session-socket-path", role]), observe::ASK_BOUND, |a| {
        let name = a.zero()?.first_line();
        if valid(&name) { Ok(name) } else { Err(Why::Malformed(format!("no usable name ('{name}')"))) }
    })
}

/// The endpoint the run talks to: with none, it stops before it writes, so a cut is never "no daemon here".
fn endpoint_name(asked: Fact<String>) -> Result<String, verdict::Stop> {
    asked.or(|why| verdict::Stop::new(1, format!("the daemon's endpoint could not be named ({why}); nothing was changed")))
}

/// Which way an install moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Newer,
    Same,
    Older,
}

/// Which way `sotd update direction` of the pack says the install at `prefix` moves; unknown when it cannot compare
/// (it prints `unknown`, or fails) or gives no answer. `installed` stands in only where the record names no version.
fn direction(m: &dyn Machine, sotd: &Path, version: &str, prefix: &Path, installed: &str) -> Fact<Direction> {
    let mut c = Command::new(sotd);
    c.args(["update", "direction", version, "--prefix"]).arg(prefix);
    let recorded = record::string(&record::read(&prefix.join("install.json")), "version").is_some_and(|v| !v.is_empty());
    if !recorded && !installed.is_empty() {
        c.args(["--installed", installed]);
    }
    observe::ask(m, &mut c, observe::ASK_BOUND, |a| match a.zero()?.first_line().as_str() {
        "newer" => Ok(Direction::Newer),
        "same" => Ok(Direction::Same),
        "older" => Ok(Direction::Older),
        other => Err(Why::Malformed(format!("'{other}', not newer, same or older"))),
    })
}

/// What a build of the line (a `+` in its version) is told about the release a run found, by ancestry and not version
/// text: its commit (the record's, else the `+<sha>` of its version) against the pack's `COMMIT`, in the install's own
/// base clone, read and never fetched into.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Way {
    Forward,
    Older,
    /// The clone answered, and what it holds cannot tell (why).
    Untold(String),
}

fn build_direction(m: &dyn Machine, prefix: &Path, installed: &str, release_commit: &str) -> Fact<Way> {
    let base = prefix.join("repo").join("base");
    let recorded = record::string(&record::read(&prefix.join("install.json")), "commit").unwrap_or_default();
    let built = if is_commit(&recorded) {
        recorded
    } else {
        installed.split_once('+').map_or("", |(_, b)| b).trim_end_matches("-dirty").to_string()
    };
    if built.is_empty() || built == "src" {
        return Fact::known(Way::Untold("this build names no commit".into()));
    }
    if !base.join(".git").exists() && !base.join("HEAD").is_file() {
        return Fact::known(Way::Untold(format!("no base clone at {}", base.display())));
    }
    // Read only: the run that asks stops, and a stop writes nothing under the prefix, so a commit the clone lacks is
    // not fetched into it. The clone is blobless, and asking it for an object it lacks fetches that object from its
    // promisor remote; the commits its refs reach are all there, so they are listed and looked up, never asked for.
    let listed = observe::ask(m, cmd::git_in(&base).args(["rev-list", "--all"]), observe::ASK_BOUND, |a| Ok(a.zero()?.out.lines().map(|l| l.trim().to_string()).collect::<Vec<_>>()));
    // `merge-base --is-ancestor`: exit 0 yes, 1 no; any other exit is no answer.
    let ancestor = |a: &str, b: &str| {
        observe::ask(m, cmd::git_in(&base).args(["merge-base", "--is-ancestor", a, b]), observe::ASK_BOUND, |ans| match ans.exit {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(ans.failed()),
        })
    };
    listed.read(
        |listed| {
            if !listed.iter().any(|l| l == release_commit) {
                return Fact::known(Way::Untold(format!("the release's commit is not in {} yet", base.display())));
            }
            let Some(built) = listed.iter().find(|l| l.starts_with(built.as_str())).cloned() else {
                return Fact::known(Way::Untold(format!("this build's commit {} is not in {}", &built[..built.len().min(9)], base.display())));
            };
            ancestor(&built, release_commit).read(
                |forward| {
                    if forward {
                        return Fact::known(Way::Forward);
                    }
                    ancestor(release_commit, &built).map(|back| if back { Way::Older } else { Way::Untold("neither the release nor this build contains the other".into()) })
                },
                Fact::unknown,
            )
        },
        Fact::unknown,
    )
}

/// The stop of a run that names no target over a build of the line: it says which way the release found moves it.
fn build_stop(installed: &str, tag: &str, way: Fact<Way>, archive_flag: &str, version_flag: &str) -> String {
    let advice = way.read(
        |way| match way {
            Way::Forward => format!("the newest release, {tag}, contains this build: naming it with {version_flag} moves this computer forward"),
            Way::Older => format!("the newest release, {tag}, does not contain this build: naming it with {version_flag} moves this computer back"),
            Way::Untold(why) => format!("could not tell which way the newest release, {tag}, moves this computer ({why})"),
        },
        |why| format!("could not tell which way the newest release, {tag}, moves this computer ({why})"),
    );
    format!("this computer runs {installed}, a build of the line, not a release: install a pack of the line with {archive_flag} <folder>; {advice}; ask the person who runs it which they want")
}

/// Never backwards unasked: a run that names no target refuses a target older than the install, or one it cannot
/// compare while the installed version is readable. A direction the run goes on without is said in its verdict, and is
/// never `Same` (so never a repair). `flag` is this system's spelling of `--version`.
#[allow(clippy::too_many_arguments, reason = "the decision's inputs, each read once")]
fn guard_direction(implicit: bool, installed: &str, dir: Fact<Direction>, version: &str, prefix: &Path, flag: &str, found: &mut verdict::Found) -> Result<Option<Direction>, verdict::Stop> {
    let tag = format!("v{version}");
    dir.read(
        |dir| {
            if implicit && dir == Direction::Older {
                return Err(verdict::Stop::at("direction", 2, format!("this computer runs {installed}, which is newer than {version}, the release this run found; it never moves back unless told: name the version you want with {flag} {tag}")));
            }
            Ok(Some(dir))
        },
        |why| {
            if implicit && !installed.is_empty() {
                return Err(verdict::Stop::at("direction", 2, format!("could not tell which way {tag} moves the install at {} (it records {installed}; {why}); a run that names no target never moves it unasked: name the version you want with {flag} {tag}", prefix.display())));
            }
            found.checked(format!("could not tell which way {tag} moved the install at {} ({why})", prefix.display()));
            Ok(None)
        },
    )
}

/// How an install meets its prefix: laid down fresh (no installed binary to update), or an update of the one there.
#[derive(Debug, PartialEq, Eq)]
enum Meet {
    Fresh,
    Update,
}

/// The way an install meets its prefix, the direction guarded first whenever a version can be read (the record's,
/// else the binary's), so a record whose binary is missing is never laid down backwards unasked.
#[cfg_attr(unix, allow(dead_code, reason = "Windows lays a prefix down fresh or updates it; Unix lays it the same way either time"))]
fn meet(installed: &str, binary: bool, guard: impl FnOnce() -> Result<(), verdict::Stop>) -> Result<Meet, verdict::Stop> {
    if !installed.is_empty() || binary {
        guard()?;
    }
    Ok(if binary { Meet::Update } else { Meet::Fresh })
}

pub(super) fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The variables the run reads that the bootstrap sets: the tag its prelude resolved, and whether the person named it.
pub(super) fn implicit_run(env: &Env) -> bool {
    env.text("SOT_INSTALL_IMPLICIT") == "1"
}

pub(super) fn bootstrap_tag(env: &Env) -> Option<String> {
    Some(env.text("SOT_INSTALL_TAG")).filter(|t| !t.is_empty())
}

/// The URL the base clone is cloned from: `SOT_INSTALL_ORIGIN` (a test's local origin), else the release repository's
/// (`SOT_INSTALL_REPO`, else the project's).
pub(super) fn origin(env: &Env) -> String {
    let repo = Some(env.text("SOT_INSTALL_REPO")).filter(|r| !r.trim().is_empty()).unwrap_or_else(|| sot_updater::identity::DEFAULT_REPO.to_string());
    Some(env.text("SOT_INSTALL_ORIGIN")).filter(|o| !o.is_empty()).unwrap_or_else(|| format!("https://github.com/{repo}"))
}

/// A bound in seconds from the environment, else `default`.
pub(super) fn secs_env(env: &Env, name: &str, default: u64) -> std::time::Duration {
    std::time::Duration::from_secs(env.text(name).trim().parse().unwrap_or(default))
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
