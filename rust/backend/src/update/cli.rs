//! `sotd update <verb>`: the offline commands that arm or judge a pointer, answered before daemon startup.
//!
//! - `stage <folder> --prefix <prefix>`: stage, prepare and arm a packed build for an install's converge. The operator
//!   chose the folder, so it is the one writer that may arm a build at or below the install (`Writer::Operator`).
//! - `pointer --prefix <prefix> --target <target>`: may the armed pointer apply onto this install? The apply scripts
//!   (sot-apply.sh, sot-apply.ps1) ask the installed `sotd` before they apply anything, so the comparison of versions
//!   and installs is Rust's `compare_versions` and not a second semver in shell. It reads the pointer, `install.json` and
//!   the stage's `prepared.json`, and writes nothing.
//!
//! - `direction <version> --prefix <prefix>`: does the install at <prefix> move `newer`, `same` or `older` to <version>?
//!   The installer (scripts/install.sh) asks the target release's own `sotd` before it changes anything, so its
//!   never-backwards default is Rust's `compare_versions` and not a second semver in shell.
//! - `repls --endpoint <unix:PATH|pipe:PATH>`: what a restart of the daemon there would end, from the daemon's own
//!   `workspace.list`: the rows with a live REPL and whether its Pluto server is running (a restart ends every notebook
//!   open in it). The installer asks before an update would restart that daemon.
//!
//! The pointer verb's exit code is the contract. 0 means the script goes on to its own checks (no pointer, a pointer
//! that fits, or no install here to compare with); `REFUSED` means drop the pointer, one that cannot be read included, and
//! one armed over an install whose `install.json` cannot be read.
//! An older `sotd` exits 1 on the unknown word
//! `update` and a `sotd update` that predates this verb exits 2 on bad arguments, so a refusal is neither: any other
//! answer leaves the script on today's exact-tag check.

use std::path::{Path, PathBuf};

use sot_updater::identity::repo_from_env;
use sot_updater::pending::{self, InstalledBuild, Writer};
use sot_updater::prepare::PreparedState;
use sot_updater::{Fetcher, InstallFile, InstallManifest, ReleaseIdentity, UpdaterConfig};

use super::refusal::fit_refusal;
use super::{stage_prepare_arm_with, Pipeline};

pub(crate) const USAGE: &str = "\
Usage: sotd update stage <folder> --prefix <install prefix>

  Stage, prepare and arm the release in <folder> (SHA256SUMS, the archive for this platform, COMMIT: what
  scripts/pack-build.sh and scripts/pack-build.ps1 write) for the install at <prefix>; the supervisor's converge
  applies it. The operator chose the folder, so there is no release-build guard and no newer-than test. A dev build
  prepares the checkout at its COMMIT, which has no tag. Prints `armed <tag> for <target> (installed <v>: newer, same or older)` and exits 0, or names why
  and exits 1.

Usage: sotd update pointer --prefix <install prefix> --target <release target>

  May the pointer armed for <target> apply onto the install at <prefix>? The apply scripts ask before they apply it.
  Reads the pointer, install.json and the stage's prepared.json; starts no daemon and writes nothing. Prints `ok` and
  exits 0 (no pointer, a pointer that fits, or no install here to compare with), or prints `refuse: <why>` and exits
  3: the pointer cannot be read, was armed over an install that is no longer the one on disk (or that install.json
  cannot be read to check), or sits at or below the installed version (a pointer with no record never applies onto a
  dev build), or its stage records no prepare, or a prepare without the Julia environments where the install's role
  runs a backend (never for a windows target, whose launcher instantiates them at the first start).

Usage: sotd update direction <version> --prefix <install prefix> [--installed <version>]

  Which way does the install at <prefix> move to <version> (a tag or a bare version)? Prints `newer`, `same` or `older`
  against the version install.json records and exits 0. An install.json that cannot be parsed still gives its version when
  the text holds one; when it holds none, prints `unknown` and exits 0. Exits 1 when install.json is absent or records no
  version and --installed gives none. --installed gives the version the caller read from the installed binary, used only where the record names none
  (or is absent).
  Starts no daemon and writes nothing.

Usage: sotd update repls --endpoint <unix:PATH|pipe:PATH>

  What would a restart of the daemon answering there end? A live REPL (the row's `repl_state` is `ready`: a Julia
  process holding the row's variables) or the Pluto server, when it is running (every notebook open in it ends with
  it). Asks the daemon's `workspace.list`; starts nothing and writes nothing. Prints nothing and exits 0 when there is
  none; prints one line each, `repl <row label>` or `pluto server`, and exits 3 when there are some; exits 1, naming
  why on stderr, when the daemon cannot be asked or cannot say whether its Pluto server is running (the `repl` lines
  it could read are printed first).";

/// `sotd update stage <folder> --prefix <prefix>`: the folder and prefix, or the reason the arguments are not those.
fn parse_stage_args(args: &[String]) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let mut rest = args.iter();
    if rest.next().map(String::as_str) != Some("stage") {
        return Err("expected `stage`".into());
    }
    let (mut folder, mut prefix) = (None, None);
    while let Some(a) = rest.next() {
        if a == "--prefix" {
            prefix = Some(rest.next().ok_or("--prefix needs a folder")?);
        } else if folder.replace(a).is_some() {
            return Err(format!("unexpected argument {a:?}"));
        }
    }
    Ok((
        folder.ok_or("no folder given")?.into(),
        prefix.ok_or("no --prefix given")?.into(),
    ))
}

/// The line `update stage` prints on success: the direction against the installed version, so a downgrade is never silent.
fn armed_line(id: &ReleaseIdentity, installed: Option<&str>) -> String {
    let armed = format!("armed {} for {}", id.tag, id.target);
    let Some(installed) = installed else { return armed };
    let direction = direction_word(&id.version, installed);
    format!("{armed} (installed v{}: {direction})", sot_updater::semver::strip_v(installed))
}

/// Which way an install at `installed` moves to `target`: `newer`, `same` or `older`.
fn direction_word(target: &str, installed: &str) -> &'static str {
    match sot_updater::semver::compare_versions(target, installed) {
        std::cmp::Ordering::Greater => "newer",
        std::cmp::Ordering::Equal => "same",
        std::cmp::Ordering::Less => "older",
    }
}

/// The folder and prefix as absolute paths: the pointer records paths under the prefix, and the apply runs later from
/// the launcher's own working folder, where a relative one leads nowhere.
fn absolute_paths(folder: &std::path::Path, prefix: &std::path::Path) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    Ok((std::path::absolute(folder)?, std::path::absolute(prefix)?))
}

/// Stage → prepare → arm the one release a pack folder holds, for the install at `prefix`.
async fn stage_folder(folder: &std::path::Path, prefix: &std::path::Path) -> Result<String, String> {
    let (folder, prefix) = absolute_paths(folder, prefix).map_err(|e| format!("making the paths absolute: {e}"))?;
    let (folder, prefix) = (folder.as_path(), prefix.as_path());
    let record = prefix.join(sot_updater::manifest::INSTALL_MANIFEST);
    let mut install = match InstallManifest::load(&record) {
        InstallFile::Read(install) => install,
        // The operator's run is what repairs a record that cannot be read: it proceeds as over no record, its prefix
        // from the argument (below), and the installer then writes the record whole.
        InstallFile::Unreadable(_) => InstallManifest::bare(prefix),
        InstallFile::Absent if sot_updater::missing_record(prefix).is_some() => InstallManifest::bare(prefix),
        InstallFile::Absent => return Err(format!("reading {}: no such file", record.display())),
    };
    install.prefix = prefix.to_path_buf();
    let sums = std::fs::read_to_string(folder.join("SHA256SUMS"))
        .map_err(|e| format!("reading {}: {e}", folder.join("SHA256SUMS").display()))?;
    let entries = sot_updater::fetch::sums::parse_sums(&sums).map_err(|e| format!("{e:#}"))?;
    let target = sot_updater::platform::this_platform().ok_or("this platform is not in the release matrix")?;
    let repo = repo_from_env();
    let id = sot_updater::fetch::sums::discover(&entries, &repo, target).map_err(|e| format!("{e:#}"))?;
    let cfg = UpdaterConfig {
        repo,
        current_version: install.version.clone().unwrap_or_default(),
        fetcher: Fetcher::Dir(folder.to_path_buf()),
        updates_root: install.updates_root(),
    };
    match stage_prepare_arm_with(&cfg, &id, Some(&install), Writer::Operator).await {
        Ok(Pipeline::Armed) => Ok(armed_line(&id, install.version.as_deref())),
        Ok(Pipeline::NotArmed(outcome)) => Err(format!("{} was staged but not armed: {}", id.tag, outcome.not_armed_because().unwrap_or_default())),
        Ok(Pipeline::Staged) => Err(format!("{} was staged but not armed: no install to prepare for", id.tag)),
        Err(e) => Err(e.to_string()),
    }
}

/// `sotd update <args>`: the exit code of `pointer ...`, `direction ...`, `repls ...` or `stage ...`.
pub(crate) async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("pointer") => pointer_cli(&args[1..]).await,
        Some("direction") => direction_cli(&args[1..]),
        Some("repls") => repls_cli(&args[1..]),
        _ => stage_cli(args).await,
    }
}

/// `sotd update stage ...`: the exit code. 2 for arguments that are not `stage <folder> --prefix <prefix>`. Progress goes
/// to stderr, because a stage on a daemon box runs Julia and npm and can take tens of minutes.
async fn stage_cli(args: &[String]) -> i32 {
    let (folder, prefix) = match parse_stage_args(args) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("sotd update: {why}\n{USAGE}");
            return 2;
        }
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
    match stage_folder(&folder, &prefix).await {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(why) => {
            eprintln!("sotd update stage: {why}");
            1
        }
    }
}


/// The exit code for a pointer that must not apply. Neither 1 nor 2: see the module doc.
pub(crate) const REFUSED: i32 = 3;

/// `--prefix <prefix> --target <target>`, in either order, or the reason the arguments are not those.
fn parse_pointer_args(args: &[String]) -> Result<(PathBuf, String), String> {
    let (mut prefix, mut target) = (None, None);
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        let slot = match a.as_str() {
            "--prefix" => &mut prefix,
            "--target" => &mut target,
            other => return Err(format!("unexpected argument {other:?}")),
        };
        *slot = Some(rest.next().ok_or_else(|| format!("{a} needs a value"))?.clone());
    }
    Ok((
        prefix.ok_or("no --prefix given")?.into(),
        target.ok_or("no --target given")?,
    ))
}

/// The refusal for the pointer armed for `target` under `prefix`, or `None` when it may apply. Without an `install.json`
/// the prefix is not an install of ours and there is nothing to compare. One that exists and cannot be read refuses a
/// pointer that records the install it was armed over (it cannot be checked), and lets one with no record meet the
/// script's exact-tag check. A pointer that exists and cannot be read is refused: every pointer a Rust writer wrote
/// carries its schema, repo and version, so one that does not parse is corrupt or another program's, and the apply must
/// not guess. A pointer that fits is last checked against this host's role (`fit_refusal`): the prepare behind it was
/// decided by whichever host armed it.
async fn refusal(prefix: &Path, target: &str) -> Option<String> {
    refusal_for(prefix, target, || backend_role_here(prefix)).await
}

/// [`refusal`] with the role question asked of `wants_backend`, only once a pointer fits: the pointer's own checks come first.
async fn refusal_for(prefix: &Path, target: &str, wants_backend: impl FnOnce() -> bool) -> Option<String> {
    let updates = prefix.join("updates");
    let installed = InstalledBuild::try_read(&updates);
    if matches!(installed, Ok(None)) {
        return None;
    }
    let ptr = match pending::read(&updates, target).await {
        Ok(None) => return None,
        Ok(Some(ptr)) => ptr,
        Err(e) => return Some(format!("unreadable pointer: {e:#}")),
    };
    if let Some(why) = pending::pointer_refusal(&ptr, installed) {
        return Some(why);
    }
    let prepared = match PreparedState::read(&sot_updater::stage_dir(&updates, &ptr.identity)).await {
        Ok(state) => Ok(state.julia_instantiated),
        Err(e) => Err(format!("{e:#}")),
    };
    fit_refusal(wants_backend(), target.starts_with("windows"), prepared)
}

/// Does the role wanted for the install at `prefix` include a backend? The record is `install.json`'s `daemon` bit; one
/// that cannot be read leaves the answer to the topology and the default. On a host the topology names with neither key
/// (a guest of a shared home) the install is another host's, so the record alone decides.
fn backend_role_here(prefix: &Path) -> bool {
    let recorded = match InstallManifest::load(&prefix.join("install.json")) {
        InstallFile::Read(m) => m.daemon,
        _ => None,
    };
    if super::refusal::host_runs_no_sotd() {
        return recorded.unwrap_or(true);
    }
    super::backend_role_recorded(recorded)
}

/// `sotd update pointer ...`: 0, `REFUSED`, or 2 for arguments that are not `--prefix <prefix> --target <target>`.
async fn pointer_cli(args: &[String]) -> i32 {
    let (prefix, target) = match parse_pointer_args(args) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("sotd update: {why}\n{USAGE}");
            return 2;
        }
    };
    match refusal(&prefix, &target).await {
        Some(why) => {
            println!("refuse: {why}");
            REFUSED
        }
        None => {
            println!("ok");
            0
        }
    }
}

/// `<version> --prefix <prefix>`, in either order, or the reason the arguments are not those.
fn parse_direction_args(args: &[String]) -> Result<(String, PathBuf, Option<String>), String> {
    let (mut version, mut prefix, mut installed) = (None, None, None);
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if a == "--prefix" {
            prefix = Some(rest.next().ok_or("--prefix needs a folder")?.clone());
        } else if a == "--installed" {
            installed = Some(rest.next().ok_or("--installed needs a version")?.clone());
        } else if version.replace(a.clone()).is_some() {
            return Err(format!("unexpected argument {a:?}"));
        }
    }
    Ok((version.ok_or("no version given")?, prefix.ok_or("no --prefix given")?.into(), installed))
}

/// What `direction` answers for the install recorded at `record`: `newer`, `same` or `older`; `unknown` for an
/// install.json that cannot be parsed and holds no version text (the installers' implicit run goes on then, because the
/// run rewrites the record); or why it cannot answer (no file, or a whole record naming no version).
///
/// `read_elsewhere` is the version the caller read from the installed binary: used only where the record names none (a
/// whole record without a version, one that cannot be parsed and holds no version text, or no record at all), so a record
/// cut inside its version, or missing, cannot hide the installed one from the no-move-back guard.
fn direction_answer(version: &str, record: &Path, read_elsewhere: Option<&str>) -> Result<String, String> {
    let word = |installed: &str| direction_word(version, installed).to_string();
    match InstallManifest::load(record) {
        InstallFile::Read(m) => m
            .version
            .map(|installed| word(&installed))
            .or_else(|| read_elsewhere.map(word))
            .ok_or_else(|| "install.json records no version".to_string()),
        InstallFile::Unreadable(_) => Ok(InstallManifest::version_in_text(record)
            .map(|installed| word(&installed))
            .or_else(|| read_elsewhere.map(word))
            .unwrap_or_else(|| "unknown".to_string())),
        InstallFile::Absent => read_elsewhere
            .map(word)
            .ok_or_else(|| format!("reading {}: no such file", record.display())),
    }
}

/// `sotd update direction ...`: 0 with the word on stdout, 1 when there is no install.json or it records no version, 2 for
/// arguments that are not `<version> --prefix <prefix>`.
fn direction_cli(args: &[String]) -> i32 {
    let (version, prefix, read_elsewhere) = match parse_direction_args(args) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("sotd update: {why}\n{USAGE}");
            return 2;
        }
    };
    match direction_answer(&version, &prefix.join(sot_updater::manifest::INSTALL_MANIFEST), read_elsewhere.as_deref()) {
        Ok(word) => {
            println!("{word}");
            0
        }
        Err(why) => {
            eprintln!("sotd update direction: {why}");
            1
        }
    }
}

/// The exit code for a daemon with live REPLs. Neither 1 (cannot tell) nor 2 (bad arguments).
pub(crate) const REPLS_LIVE: i32 = 3;

/// How long the daemon may take to list its rows: it answers from memory.
const LIST_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// What a restart of the daemon would end, from a `workspace.list` reply: one line each, `repl <label>` for every row whose
/// REPL is live (its `repl_state` is `ready`, a Julia process holding the row's variables; `starting` holds none yet and
/// `dead` or `not_started` none any more) and `pluto server` when the Pluto server is running (every notebook open in it
/// ends with it, however it was opened). `unsure` is what the reply could not say: it has no `workspaces` list (an error
/// payload is one), or no `pluto_server_up` (a daemon older than the field). The lines read before that are kept, and the
/// caller must not read an unsure reply as "none live".
struct LiveWork {
    lines: Vec<String>,
    unsure: Option<String>,
}

fn live_work(reply: &serde_json::Value) -> LiveWork {
    let text = |row: &serde_json::Value, key: &str| row.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let Some(rows) = reply.get("workspaces").and_then(|v| v.as_array()) else {
        let said = reply.get("error").and_then(|v| v.as_str()).unwrap_or("no `workspaces` key");
        return LiveWork { lines: Vec::new(), unsure: Some(format!("the daemon answered without a workspace list: {said}")) };
    };
    let mut lines: Vec<String> = rows
        .iter()
        .filter(|row| row.get("repl_state").and_then(|v| v.as_str()) == Some("ready"))
        .filter_map(|row| text(row, "label").or_else(|| text(row, "slug")))
        .map(|label| format!("repl {label}"))
        .collect();
    let unsure = match reply.get("pluto_server_up").and_then(|v| v.as_bool()) {
        Some(true) => {
            lines.push("pluto server".to_string());
            None
        }
        Some(false) => None,
        None => Some("the daemon's reply says nothing of its Pluto server (a daemon older than this build cannot say whether it is running)".to_string()),
    };
    LiveWork { lines, unsure }
}

/// `--endpoint <unix:PATH|pipe:PATH>`, or the reason the arguments are not that.
fn parse_repls_args(args: &[String]) -> Result<String, String> {
    match args {
        [flag, ep] if flag == "--endpoint" && (ep.starts_with("unix:") || ep.starts_with("pipe:")) => Ok(ep.clone()),
        _ => Err("expected --endpoint <unix:PATH|pipe:PATH>".into()),
    }
}

/// `sotd update repls ...`: 0 (none live), `REPLS_LIVE` (the `repl` and `pluto` lines on stdout), 1 (the daemon cannot be
/// asked) or 2 (bad arguments).
fn repls_cli(args: &[String]) -> i32 {
    let endpoint = match parse_repls_args(args) {
        Ok(ep) => ep,
        Err(why) => {
            eprintln!("sotd update: {why}\n{USAGE}");
            return 2;
        }
    };
    let asked = sot_log::host::state_dir::host_name().and_then(|me| {
        crate::topology::dial::dial_and_call(&endpoint, &me, sot_protocol::op::WORKSPACE_LIST, serde_json::json!({}), LIST_BOUND)
    });
    let reply = match asked {
        Ok(reply) => reply,
        Err(why) => {
            eprintln!("sotd update repls: {why}");
            return 1;
        }
    };
    let work = live_work(&reply);
    for line in &work.lines {
        println!("{line}");
    }
    if let Some(why) = &work.unsure {
        eprintln!("sotd update repls: {why}");
    }
    repls_exit(&work)
}

/// The exit code for what was read: 1 when the reply left something unsaid (whatever lines it did give), else 0 or `REPLS_LIVE`.
fn repls_exit(work: &LiveWork) -> i32 {
    match (&work.unsure, work.lines.is_empty()) {
        (Some(_), _) => 1,
        (None, true) => 0,
        (None, false) => REPLS_LIVE,
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
