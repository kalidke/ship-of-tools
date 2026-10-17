//! `sotd update <verb>`: the offline commands that arm or judge a pointer, answered before daemon startup.
//!
//! - `stage <folder> --prefix <prefix>`: stage, prepare and arm a packed build for an install's converge. The operator
//!   chose the folder, so it is the one writer that may arm a build at or below the install (`Writer::Operator`).
//! - `pointer --prefix <prefix> --target <target>`: may the armed pointer apply onto this install? The apply scripts
//!   (sot-apply.sh, sot-apply.ps1) ask the installed `sotd` before they apply anything, so the comparison of versions
//!   and installs is Rust's `compare_versions` and not a second semver in shell. It reads two small files and writes
//!   nothing.
//!
//! - `direction <version> --prefix <prefix>`: does the install at <prefix> move `newer`, `same` or `older` to <version>?
//!   The installer (scripts/install.sh) asks the target release's own `sotd` before it changes anything, so its
//!   never-backwards default is Rust's `compare_versions` and not a second semver in shell.
//! - `repls --endpoint <unix:PATH|pipe:PATH>`: what a restart of the daemon there would end, from the daemon's own
//!   `workspace.list`: the rows with a live REPL and the notebooks its Pluto server has open. The installer asks before
//!   an update would restart that daemon.
//!
//! The pointer verb's exit code is the contract. 0 means the script goes on to its own checks (no pointer, a pointer
//! that fits, or no install here to compare with); `REFUSED` means drop the pointer, one that cannot be read included.
//! An older `sotd` exits 1 on the unknown word
//! `update` and a `sotd update` that predates this verb exits 2 on bad arguments, so a refusal is neither: any other
//! answer leaves the script on today's exact-tag check.

use std::path::{Path, PathBuf};

use sot_updater::identity::repo_from_env;
use sot_updater::pending::{self, InstalledBuild, Writer};
use sot_updater::{Fetcher, InstallManifest, ReleaseIdentity, UpdaterConfig};

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
  Reads the pointer and install.json; starts no daemon and writes nothing. Prints `ok` and exits 0 (no pointer, a
  pointer that fits, or no install here to compare with), or prints `refuse: <why>` and exits 3: the pointer cannot be
  read, was armed over an install that is no longer the one on disk, or sits at or below the installed version (a
  pointer with no record never applies onto a dev build).

Usage: sotd update direction <version> --prefix <install prefix>

  Which way does the install at <prefix> move to <version> (a tag or a bare version)? Prints `newer`, `same` or `older`
  against the version install.json records and exits 0; exits 1 when install.json cannot be read or records no version.
  Starts no daemon and writes nothing.

Usage: sotd update repls --endpoint <unix:PATH|pipe:PATH>

  What would a restart of the daemon answering there end? A live REPL (the row's `repl_state` is `ready`: a Julia
  process holding the row's variables) or a notebook its Pluto server has open. Asks the daemon's `workspace.list`;
  starts nothing and writes nothing. Prints nothing and exits 0 when there is none; prints one line each, `repl <row
  label>` or `pluto <notebook path>`, and exits 3 when there are some; exits 1, naming why on stderr, when the daemon
  cannot be asked, or cannot say which notebooks are open.";

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
    let mut install = InstallManifest::read(&prefix.join(sot_updater::manifest::INSTALL_MANIFEST)).map_err(|e| format!("{e:#}"))?;
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
        Ok(_) => Err(format!("{} was staged but not armed: a newer release is armed, or this one is marked bad", id.tag)),
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

/// The refusal for the pointer armed for `target` under `prefix`, or `None` when it may apply. Without a readable
/// `install.json` the prefix is not an install of ours and there is nothing to compare. A pointer that exists and cannot
/// be read is refused: every pointer a Rust writer wrote carries its schema, repo and version, so one that does not
/// parse is corrupt or another program's, and the apply must not guess.
async fn refusal(prefix: &Path, target: &str) -> Option<String> {
    let updates = prefix.join("updates");
    let installed = InstalledBuild::read(&updates)?;
    match pending::read(&updates, target).await {
        Ok(None) => None,
        Ok(Some(ptr)) => pending::stale_reason(&ptr, &installed),
        Err(e) => Some(format!("unreadable pointer: {e:#}")),
    }
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
fn parse_direction_args(args: &[String]) -> Result<(String, PathBuf), String> {
    let (mut version, mut prefix) = (None, None);
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if a == "--prefix" {
            prefix = Some(rest.next().ok_or("--prefix needs a folder")?.clone());
        } else if version.replace(a.clone()).is_some() {
            return Err(format!("unexpected argument {a:?}"));
        }
    }
    Ok((version.ok_or("no version given")?, prefix.ok_or("no --prefix given")?.into()))
}

/// `sotd update direction ...`: 0 with the word on stdout, 1 when the install's version cannot be read, 2 for arguments
/// that are not `<version> --prefix <prefix>`.
fn direction_cli(args: &[String]) -> i32 {
    let (version, prefix) = match parse_direction_args(args) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("sotd update: {why}\n{USAGE}");
            return 2;
        }
    };
    let installed = InstallManifest::read(&prefix.join(sot_updater::manifest::INSTALL_MANIFEST))
        .map_err(|e| format!("{e:#}"))
        .and_then(|m| m.version.ok_or_else(|| "install.json records no version".to_string()));
    match installed {
        Ok(installed) => {
            println!("{}", direction_word(&version, &installed));
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

/// What a restart of the daemon would end, from a `workspace.list` reply, one line each: `repl <label>` for every row whose
/// REPL is live (its `repl_state` is `ready`, a Julia process holding the row's variables; `starting` holds none yet and
/// `dead` or `not_started` none any more) and `pluto <path>` for every notebook the Pluto server has open. A reply with no
/// `workspaces` list (an error payload is one), or from a daemon that cannot list its notebooks (no `pluto_notebooks`),
/// is no answer: the caller must not read it as "none live".
fn live_work(reply: &serde_json::Value) -> Result<Vec<String>, String> {
    let text = |row: &serde_json::Value, key: &str| row.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let rows = reply.get("workspaces").and_then(|v| v.as_array()).ok_or_else(|| {
        let said = reply.get("error").and_then(|v| v.as_str()).unwrap_or("no `workspaces` key");
        format!("the daemon answered without a workspace list: {said}")
    })?;
    let notebooks = reply.get("pluto_notebooks").and_then(|v| v.as_array()).ok_or_else(|| {
        "the daemon's reply lists no Pluto notebooks (a daemon older than this build cannot say which are open)".to_string()
    })?;
    let repls = rows
        .iter()
        .filter(|row| row.get("repl_state").and_then(|v| v.as_str()) == Some("ready"))
        .filter_map(|row| text(row, "label").or_else(|| text(row, "slug")))
        .map(|label| format!("repl {label}"));
    let sessions = notebooks.iter().filter_map(|path| path.as_str()).map(|path| format!("pluto {path}"));
    Ok(repls.chain(sessions).collect())
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
    match asked.and_then(|reply| live_work(&reply)) {
        Ok(rows) if rows.is_empty() => 0,
        Ok(rows) => {
            for row in rows {
                println!("{row}");
            }
            REPLS_LIVE
        }
        Err(why) => {
            eprintln!("sotd update repls: {why}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_identity(tag: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            repo: "kalidke/ship-of-tools".into(),
            tag: tag.into(),
            version: tag.trim_start_matches('v').into(),
            target: "linux-x86_64".into(),
            asset: format!("sot-{}-linux-x86_64.tar.gz", tag.trim_start_matches('v')),
            asset_sha256: "0".repeat(64),
        }
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[tokio::test]
    async fn stage_cli_without_its_folder_or_prefix_exits_2() {
        for bad in ["", "stage", "stage d", "stage --prefix p", "stage d e --prefix p", "stage d --prefix", "other d --prefix p"] {
            assert_eq!(stage_cli(&argv(bad)).await, 2, "{bad:?}");
        }
    }

    #[test]
    fn stage_args_take_the_folder_and_the_prefix_in_either_order() {
        let want = Ok(("d".into(), "p".into()));
        assert_eq!(parse_stage_args(&argv("stage d --prefix p")), want);
        assert_eq!(parse_stage_args(&argv("stage --prefix p d")), want);
    }

    #[test]
    fn stage_paths_are_absolute_before_anything_records_them() {
        let cwd = std::env::current_dir().unwrap();
        let (folder, prefix) = absolute_paths("d".as_ref(), "p/q".as_ref()).unwrap();
        assert_eq!((folder, prefix), (cwd.join("d"), cwd.join("p/q")));
        assert_eq!(absolute_paths(&cwd, &cwd).unwrap(), (cwd.clone(), cwd));
    }

    #[test]
    fn the_armed_line_says_which_way_the_install_moves() {
        let line = |installed: Option<&str>, tag: &str| armed_line(&fake_identity(tag), installed);
        assert_eq!(line(Some("0.6.6-rc9.13"), "v0.6.5"), "armed v0.6.5 for linux-x86_64 (installed v0.6.6-rc9.13: older)");
        assert_eq!(line(Some("0.6.5"), "v0.6.6-rc9.13"), "armed v0.6.6-rc9.13 for linux-x86_64 (installed v0.6.5: newer)");
        assert_eq!(line(Some("0.6.6-rc9.14-dev+aaa"), "v0.6.6-rc9.14-dev+bbb"), "armed v0.6.6-rc9.14-dev+bbb for linux-x86_64 (installed v0.6.6-rc9.14-dev+aaa: same)");
        assert_eq!(line(None, "v0.6.5"), "armed v0.6.5 for linux-x86_64");
    }

    const T: &str = "linux-x86_64";

    /// A prefix holding an `install.json` for `installed` and, when given, a pointer for `tag` that records `over`.
    fn prefix(installed: (&str, &str), pointer: Option<(&str, Option<(&str, &str)>)>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("updates")).unwrap();
        std::fs::write(
            dir.path().join("install.json"),
            format!(r#"{{"schema":1,"prefix":"/p","version":"{}","commit":"{}"}}"#, installed.0, installed.1),
        )
        .unwrap();
        if let Some((version, over)) = pointer {
            let over = over.map_or(String::new(), |(v, c)| format!(r#","armed_over_version":"{v}","armed_over_commit":"{c}""#));
            std::fs::write(
                dir.path().join("updates").join(format!("pending-{T}.json")),
                format!(
                    r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"v{version}","version":"{version}","target":"{T}",
                    "asset":"sot-{version}-{T}.tar.gz","asset_sha256":"{}","checkout":"/x","commit":"cc","armed_at":1{over}}}"#,
                    "ab".repeat(32)
                ),
            )
            .unwrap();
        }
        dir
    }

    async fn code(dir: &tempfile::TempDir) -> i32 {
        pointer_cli(&["--prefix".into(), dir.path().display().to_string(), "--target".into(), T.into()]).await
    }

    #[test]
    fn arguments_are_the_prefix_and_the_target_in_either_order() {
        let want = Ok(("p".into(), "t".into()));
        assert_eq!(parse_pointer_args(&argv("--prefix p --target t")), want);
        assert_eq!(parse_pointer_args(&argv("--target t --prefix p")), want);
        for bad in ["", "--prefix p", "--target t", "--prefix p --target", "--prefix p --target t x", "stage"] {
            assert!(parse_pointer_args(&argv(bad)).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn bad_arguments_exit_2_and_a_refusal_is_neither_1_nor_2() {
        assert_eq!(pointer_cli(&argv("--prefix p")).await, 2);
        assert!(REFUSED != 1 && REFUSED != 2);
    }

    #[tokio::test]
    async fn a_pointer_armed_over_the_install_on_disk_may_apply() {
        let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")))));
        assert_eq!(code(&dir).await, 0);
    }

    #[tokio::test]
    async fn a_pointer_armed_over_another_install_is_refused() {
        let dir = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.13", Some(("0.6.6-rc9.12", "c0")))));
        assert_eq!(code(&dir).await, REFUSED);
    }

    // The stale pointer: armed by a daemon before the record existed, below the install it would land on.
    #[tokio::test]
    async fn a_pointer_with_no_record_applies_only_above_the_install() {
        let below = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.13", None)));
        assert_eq!(code(&below).await, REFUSED);
        let at = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.13", None)));
        assert_eq!(code(&at).await, REFUSED);
        let above = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", None)));
        assert_eq!(code(&above).await, 0);
    }

    #[tokio::test]
    async fn nothing_armed_or_no_install_here_leaves_the_script_to_its_own_checks() {
        assert_eq!(code(&prefix(("0.6.6-rc9.13", "c0"), None)).await, 0);
        let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.13", None)));
        std::fs::remove_file(dir.path().join("install.json")).unwrap();
        assert_eq!(code(&dir).await, 0, "no install.json: not an install of ours");
    }

    #[tokio::test]
    async fn a_pointer_that_cannot_be_read_is_refused_with_the_error() {
        let garbled = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", None)));
        let path = garbled.path().join("updates").join(format!("pending-{T}.json"));
        let why = |text: &[u8]| {
            std::fs::write(&path, text).unwrap();
            refusal(garbled.path(), T)
        };
        let asha = "ab".repeat(32);
        let full = |tag: &str, version: &str| {
            format!(
                r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"{tag}","version":"{version}","target":"{T}","asset":"sot-{version}-{T}.tar.gz","asset_sha256":"{asha}","checkout":"/x","commit":"cc","armed_at":1}}"#
            )
        };
        let cases = [
            ("not json", "{not json".to_string()),
            ("no schema, repo or version", format!(r#"{{"tag":"v0.6.6-rc9.14","target":"{T}"}}"#)),
            ("no version", full("v0.6.6-rc9.14", "0.6.6-rc9.14").replace(r#""version":"0.6.6-rc9.14","#, "")),
            ("tag and version disagree", full("v0.6.6-rc9.14", "0.6.6-rc9.15")),
        ];
        for (name, text) in cases {
            let why = why(text.as_bytes()).await.unwrap_or_else(|| panic!("{name}: not refused"));
            assert!(why.starts_with("unreadable pointer: "), "{name}: {why}");
        }
        assert_eq!(code(&garbled).await, REFUSED);
        // The same pointer, whole and above the install, is read and fits.
        assert!(why(full("v0.6.6-rc9.14", "0.6.6-rc9.14").as_bytes()).await.is_none());
    }

    // The review's measurement: a no-record release pointer above a dev stamp used to be answered `ok`.
    #[tokio::test]
    async fn a_pointer_with_no_record_never_applies_onto_a_dev_build() {
        let dir = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.14", None)));
        assert_eq!(code(&dir).await, REFUSED);
        let operator = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c1")))));
        assert_eq!(code(&operator).await, 0, "an operator's pointer carries its record");
    }

    #[test]
    fn direction_arguments_are_the_version_and_the_prefix_in_either_order() {
        let want = Ok(("v0.6.6".to_string(), "p".into()));
        assert_eq!(parse_direction_args(&argv("v0.6.6 --prefix p")), want);
        assert_eq!(parse_direction_args(&argv("--prefix p v0.6.6")), want);
        for bad in ["", "v0.6.6", "--prefix p", "v0.6.6 --prefix", "a b --prefix p"] {
            assert!(parse_direction_args(&argv(bad)).is_err(), "{bad:?}");
            assert_eq!(direction_cli(&argv(bad)), 2, "{bad:?}");
        }
    }

    #[test]
    fn update_direction_names_the_way_the_install_moves() {
        // installed, target, the word. Dev rows included: a main build sits above the candidate it follows.
        let table = [
            ("0.6.6-rc9.14-dev+a", "0.6.6-rc9.13", "older"),
            ("0.6.6-rc9.14-dev+a", "0.6.6-rc9.14", "newer"),
            ("0.6.6-rc9.14-dev+a", "v0.6.6-rc9.14-dev+a", "same"),
            ("0.6.6-rc9.13", "v0.6.6-rc9.13", "same"),
            ("0.6.6-rc9.13", "0.6.5", "older"),
            ("0.6.5", "v0.6.6-rc9.13", "newer"),
            ("0.6.6-rc9.13", "v0.6.6", "newer"),
        ];
        for (installed, target, want) in table {
            assert_eq!(direction_word(target, installed), want, "{installed} -> {target}");
            let dir = prefix((installed, "c0"), None);
            let got = direction_cli(&["--prefix".into(), dir.path().display().to_string(), target.into()]);
            assert_eq!(got, 0, "{installed} -> {target}");
        }
    }

    #[test]
    fn direction_without_a_readable_install_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let ask = |d: &std::path::Path| direction_cli(&["--prefix".into(), d.display().to_string(), "v0.6.6".into()]);
        assert_eq!(ask(dir.path()), 1, "no install.json");
        std::fs::write(dir.path().join("install.json"), "{not json").unwrap();
        assert_eq!(ask(dir.path()), 1, "garbled install.json");
        std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p"}"#).unwrap();
        assert_eq!(ask(dir.path()), 1, "no version recorded");
    }

    #[test]
    fn a_live_repl_is_one_the_daemon_says_is_ready() {
        let reply = serde_json::json!({ "pluto_notebooks": [], "workspaces": [
            { "workspace_id": "ws-a", "slug": "rig", "label": "Rig PC", "repl_state": "ready" },
            { "workspace_id": "ws-b", "slug": "idle", "label": "Idle", "repl_state": "not_started" },
            { "workspace_id": "ws-c", "slug": "boot", "label": "Boot", "repl_state": "starting" },
            { "workspace_id": "ws-d", "slug": "gone", "label": "Gone", "repl_state": "dead" },
            { "workspace_id": "ws-e", "slug": "scan", "label": "", "repl_state": "ready" },
            { "workspace_id": "ws-f", "slug": "old" },
        ]});
        assert_eq!(live_work(&reply), Ok(vec!["repl Rig PC".to_string(), "repl scan".to_string()]));
        assert_eq!(live_work(&serde_json::json!({ "workspaces": [], "pluto_notebooks": [] })), Ok(vec![]));
    }

    #[test]
    fn a_notebook_the_pluto_server_has_open_is_live_work_too() {
        let reply = serde_json::json!({
            "workspaces": [{ "slug": "rig", "label": "Rig PC", "repl_state": "ready" }],
            "pluto_notebooks": ["/p/a.jl", "/p/b.jl"],
        });
        let live = ["repl Rig PC", "pluto /p/a.jl", "pluto /p/b.jl"].map(String::from).to_vec();
        assert_eq!(live_work(&reply), Ok(live));
        let only = serde_json::json!({ "workspaces": [], "pluto_notebooks": ["/p/a.jl"] });
        assert_eq!(live_work(&only), Ok(vec!["pluto /p/a.jl".to_string()]));
    }

    #[test]
    fn a_daemon_that_cannot_list_its_notebooks_has_not_answered() {
        // A daemon older than the field lists rows only: it may hold Pluto sessions it cannot name.
        let old = serde_json::json!({ "workspaces": [] });
        assert_eq!(
            live_work(&old),
            Err("the daemon's reply lists no Pluto notebooks (a daemon older than this build cannot say which are open)".to_string())
        );
    }

    #[test]
    fn a_reply_with_no_rows_is_not_an_answer() {
        // An error payload is a reply too; reading it as "no live REPL" would restart the daemon unasked.
        let refused = serde_json::json!({ "error": "unknown op" });
        assert_eq!(live_work(&refused), Err("the daemon answered without a workspace list: unknown op".to_string()));
        assert!(live_work(&serde_json::json!({})).is_err());
        assert!(live_work(&serde_json::json!({ "workspaces": "none", "pluto_notebooks": [] })).is_err());
        assert!(live_work(&serde_json::json!({ "workspaces": [], "pluto_notebooks": "none" })).is_err());
    }

    #[test]
    fn repls_takes_one_endpoint_and_a_daemon_that_does_not_answer_is_exit_1() {
        assert_eq!(parse_repls_args(&argv("--endpoint unix:/x")), Ok("unix:/x".to_string()));
        for bad in ["", "--endpoint", "--endpoint /x", "unix:/x", "--endpoint unix:/x y"] {
            assert!(parse_repls_args(&argv(bad)).is_err(), "{bad:?}");
            assert_eq!(repls_cli(&argv(bad)), 2, "{bad:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let none = format!("--endpoint unix:{}/absent.sock", dir.path().display());
        assert_eq!(repls_cli(&argv(&none)), 1);
    }
}
