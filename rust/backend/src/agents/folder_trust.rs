// folder_trust.rs — the folder-trust record the daemon pre-answers for a declared root prefix.

use std::path::{Path, PathBuf};

use super::accounts::claude_config_dir;

/// Claude Code's own per-folder trust record, a JSON object keyed by
/// absolute project path under `projects`, each entry carrying
/// `hasTrustDialogAccepted` (see [`ensure_folder_trusted`]).
const CLAUDE_TRUST_FILE: &str = ".claude.json";
/// This box's own settings file, in the daemon's config directory --
/// where the installed declaration lives (see below).
#[cfg(test)]
const SETTINGS_FILE: &str = "settings.toml";
/// The bool inside a `projects` entry that means "this folder's trust
/// dialog is answered" -- claude's own key name, not ours.
const TRUST_ACCEPTED_KEY: &str = "hasTrustDialogAccepted";
/// Read the typed user-level declaration at each spawn.
pub fn trusted_root_prefix() -> Result<Option<PathBuf>, String> {
    super::trust_declaration::read_trust_declaration(&super::trust_declaration::declaration_file())
}

#[cfg(test)]
fn declared_root_prefix(config_dir: &Path) -> Option<PathBuf> {
    super::trust_declaration::read_trust_declaration(&config_dir.join(SETTINGS_FILE)).unwrap()
}
#[cfg(test)]
fn parse_declared_root_prefix(text: &str) -> Option<PathBuf> {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(SETTINGS_FILE);
    std::fs::write(&path, text).unwrap();
    super::trust_declaration::read_trust_declaration(&path).unwrap()
}

/// Which `.claude.json` records trust for `account`. NOT
/// `claude_config_dir(..).join(..)` for the default account: with
/// `CLAUDE_CONFIG_DIR` unset claude keeps this file BESIDE its config
/// folder, in the home itself (`<home>/.claude/.claude.json` exists too
/// on a real box but carries no `projects` table, so writing there would
/// write a file claude never reads). A NAMED account has
/// `CLAUDE_CONFIG_DIR` set to its own folder ([`account_env`]) and the
/// file is inside it.
pub fn claude_trust_file(home: &Path, account: &str) -> PathBuf {
    if account.is_empty() || account == "default" {
        home.join(CLAUDE_TRUST_FILE)
    } else {
        claude_config_dir(home, account).join(CLAUDE_TRUST_FILE)
    }
}

/// Record `root` as trusted in `account`'s trust file when -- and only
/// when -- it lies under the owner's declared `prefix`, so a row the
/// daemon spawns does not stop at claude's folder-trust dialog for a
/// folder the owner has already declared trusted. That 2026-09-27 ruling
/// supersedes the earlier "the daemon must never pre-trust a root" rule
/// ONLY inside the declared set: outside it the older rule still stands
/// and this function writes nothing whatsoever.
///
/// Pure in the same sense as [`account_env`] -- `home` and `prefix` are
/// arguments, never read from the environment here -- so the one caller
/// ([`crate::rows::spawn::detach::spawn_detached_supervisor`])
/// resolves the declaration once and the tests need no process state.
/// Called where the config dir is prepared, which is why a first spawn
/// and a leg resumed after a reauth need no case of their own: the
/// reauth's target account has a config dir that never trusted this
/// folder, so it stops for exactly the same reason and is fixed by
/// exactly the same call.
///
/// `Ok(true)` means this call recorded the flag; `Ok(false)` that there
/// was nothing to do -- nothing declared, `root` outside the declared
/// set, or claude already trusts it. `Err` is a REPORT, never a refusal:
/// the caller logs it and starts the agent anyway. The worst acceptable
/// outcome is the owner answering the dialog by hand (what happened
/// before this existed); a row that will not start is a worse one.
///
/// `Path::starts_with` is a COMPONENT-wise test, not a string one: a
/// sibling directory whose name merely begins with the declared one is
/// outside the declared set. A `root` equal to the prefix is inside it --
/// that tree is what the owner declared.
///
/// Claude Code owns this file, so nothing here assumes it is ours: every
/// other key survives, a shape we cannot parse is refused rather than
/// replaced, and the new bytes are published with a temp file plus a
/// plain `rename` (never `renameat2`'s flags -- `RENAME_NOREPLACE` and
/// `RENAME_EXCHANGE` return `EINVAL` on an NFS home).
pub fn ensure_folder_trusted(
    home: &Path,
    account: &str,
    root: &Path,
    prefix: Option<&Path>,
) -> Result<bool, String> {
    let Some(prefix) = prefix else { return Ok(false) };
    if !prefix.is_absolute() {
        return Err(format!(
            "declared trusted-folder prefix {prefix:?} ([trust] root_prefix) is not an absolute path: nothing is trusted"
        ));
    }
    if !root.is_absolute() {
        return Err(format!("project root {root:?} is not an absolute path: nothing is trusted"));
    }
    if !root.starts_with(prefix) {
        return Ok(false);
    }

    let path = claude_trust_file(home, account);
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(at(&e)),
    };
    let mut doc = match existing.as_deref() {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes).map_err(|e| at(&e))?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    let top = doc.as_object_mut().ok_or_else(|| at(&"not a JSON object"))?;
    let projects = top
        .entry("projects")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| at(&"\"projects\" is not a JSON object"))?;
    let entry = projects
        .entry(claude_project_key(&root.to_string_lossy(), cfg!(windows)))
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| at(&"this project's own entry is not a JSON object"))?;
    // Already answered: leave the file exactly as it is rather than
    // rewriting it on every spawn -- claude may be writing it right now.
    if entry.get(TRUST_ACCEPTED_KEY).and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(false);
    }
    entry.insert(TRUST_ACCEPTED_KEY.to_string(), serde_json::Value::Bool(true));

    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| at(&e))?;
    publish_trust_file(&path, &bytes).map_err(|e| at(&e))?;
    Ok(true)
}

/// The key Claude Code files a project under in `.claude.json`. On Windows
/// its own keys read `C:/Users/...` (forward slashes), while a root may
/// arrive with `\`; spelled differently, the entry would never be found
/// and the dialog would still appear. Elsewhere the root is the key as is.
fn claude_project_key(root: &str, windows: bool) -> String {
    if windows {
        root.replace('\\', "/")
    } else {
        root.to_string()
    }
}

/// Publish [`ensure_folder_trusted`]'s bytes: temp file in the SAME
/// directory, then a plain `rename`. The temp name carries this process's
/// id so two spawns racing on one config dir -- and claude's own temp
/// file, whatever it is called -- never collide, and a failed attempt
/// leaves no litter behind. Mode is carried over from the file being
/// replaced (claude keeps it owner-only); a file we create is owner-only
/// too, never whatever the daemon's umask happens to be.
fn publish_trust_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(CLAUDE_TRUST_FILE);
    let tmp = dir.join(format!("{name}.sot-{}.tmp", std::process::id()));
    let published = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&tmp, path)
    })();
    if published.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    published
}

#[cfg(test)]
mod tests {
    use super::super::accounts::CLAUDE_ACCOUNTS_DIR;
    use super::super::support_tests::touch_dir;
    use super::*;

    #[test]
    fn claude_project_key_maps_backslashes_only_on_windows() {
        assert_eq!(claude_project_key(r"C:\Users\u\repo", true), "C:/Users/u/repo");
        assert_eq!(claude_project_key(r"C:\Users\u\repo", false), r"C:\Users\u\repo");
        assert_eq!(claude_project_key("C:/Users/u/repo", true), "C:/Users/u/repo");
        assert_eq!(claude_project_key("C:/Users/u/repo", false), "C:/Users/u/repo");
    }

    #[test]
    #[cfg(windows)]
    fn a_backslash_root_is_recorded_under_the_slash_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let text = std::fs::read_to_string(claude_trust_file(home, "")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let key = claude_project_key(&root.to_string_lossy(), true);
        assert!(!key.contains('\\'));
        assert_eq!(doc["projects"][&key][TRUST_ACCEPTED_KEY], serde_json::Value::Bool(true));
    }


    // ---- declared-folder trust (the prompt the daemon pre-answers) ----

    /// Stand-in for the parent directory the owner declares: made inside a
    /// tempdir, so no real path from any machine appears in this repo.
    fn declared_parent(home: &Path) -> PathBuf {
        let p = home.join("declared-parent");
        touch_dir(&p);
        p
    }

    /// What the trust file actually records for `root` -- `None` when the
    /// file, the `projects` table, or the entry is absent.
    fn recorded_trust(file: &Path, root: &Path) -> Option<bool> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        v.get("projects")?
            .get(claude_project_key(&root.to_string_lossy(), cfg!(windows)).as_str())?
            .get("hasTrustDialogAccepted")?
            .as_bool()
    }

    #[test]
    fn a_root_under_the_declared_prefix_is_recorded_as_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        assert_eq!(recorded_trust(&claude_trust_file(home, ""), &root), Some(true));
    }

    #[test]
    fn a_root_outside_the_declared_prefix_leaves_the_file_byte_identical() {
        // The 2026-09-03 ruling still standing where it applies: outside the
        // declared set the daemon adds no trust flag at all.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let outside = home.join("elsewhere").join("some-repo");
        touch_dir(&outside);
        let file = claude_trust_file(home, "");
        let before = br#"{"numStartups":3,"projects":{}}"#.to_vec();
        std::fs::write(&file, &before).unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &outside, Some(&parent)), Ok(false));
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    #[test]
    fn the_declared_prefix_matches_whole_path_components_only() {
        // `<parent>-other` is NOT under `<parent>`: a plain string-prefix
        // test would trust it, which is precisely the over-grant to refuse.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let sibling = PathBuf::from(format!("{}-other", parent.display())).join("some-repo");
        touch_dir(&sibling);

        assert_eq!(ensure_folder_trusted(home, "", &sibling, Some(&parent)), Ok(false));
        assert!(!claude_trust_file(home, "").exists(), "nothing may be written");
    }

    #[test]
    fn a_dot_dot_spelling_that_escapes_the_prefix_writes_an_inert_key() {
        // `Path::starts_with` is LEXICAL, so a root spelled with `..` passes
        // the prefix test although it RESOLVES outside the declared parent.
        // That is not an over-grant, and this test is what says so rather
        // than leaving it as a property nobody wrote down: the key recorded
        // is the literal spelling (`ensure_folder_trusted` keys on `root` as
        // passed), while claude looks a project up by its RESOLVED `getcwd()`.
        // So the escaping root's real path gets NO entry, the dialog still
        // appears there, and the write grants nothing it should not.
        //
        // The inertness is a property of the KEYING, not a check -- which is
        // exactly why it is pinned here. A future change that normalises the
        // key before writing it would turn this same lexical gap into a real
        // over-grant, and this test is what would go red.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let escaped = home.join("elsewhere").join("some-repo");
        touch_dir(&escaped);
        let spelled = parent.join("..").join("elsewhere").join("some-repo");

        assert!(
            spelled.starts_with(&parent),
            "the lexical prefix test PASSES for this spelling -- that is the premise of the test"
        );
        assert_eq!(ensure_folder_trusted(home, "", &spelled, Some(&parent)), Ok(true));

        let file = claude_trust_file(home, "");
        assert_eq!(
            recorded_trust(&file, &spelled),
            Some(true),
            "the literal spelling is what gets written"
        );
        assert_eq!(
            recorded_trust(&file, &escaped),
            None,
            "the resolved path -- the one claude would look up -- gets nothing, so the write is inert"
        );
    }

    #[test]
    fn recording_trust_preserves_every_other_key_and_other_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let other = "/not-under-the-prefix/another-project";
        let file = claude_trust_file(home, "");
        std::fs::write(
            &file,
            serde_json::to_vec_pretty(&serde_json::json!({
                "numStartups": 7,
                "installMethod": "native",
                "projects": {
                    other: { "hasTrustDialogAccepted": false, "allowedTools": ["Bash"] },
                },
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["numStartups"], 7);
        assert_eq!(v["installMethod"], "native");
        assert_eq!(v["projects"][other]["hasTrustDialogAccepted"], false, "another project's own answer stands");
        assert_eq!(v["projects"][other]["allowedTools"][0], "Bash");
        assert_eq!(recorded_trust(&file, &root), Some(true));
    }

    #[test]
    fn an_absent_trust_file_is_created_with_only_that_one_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        assert!(!file.exists());

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 1, "no key invented beyond projects");
        assert_eq!(v["projects"].as_object().unwrap().len(), 1);
        assert_eq!(recorded_trust(&file, &root), Some(true));
    }

    #[test]
    fn no_declared_prefix_writes_nothing_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let root = declared_parent(home).join("some-repo");
        touch_dir(&root);

        assert_eq!(ensure_folder_trusted(home, "", &root, None), Ok(false));
        assert!(
            !claude_trust_file(home, "").exists(),
            "with nothing declared the dialog is answered by hand -- today's behaviour"
        );
    }

    #[test]
    fn a_named_accounts_trust_file_is_the_one_inside_its_own_config_dir() {
        // The reauth case: another account's config dir has never trusted
        // this folder, which is why a resumed leg stops just like a first
        // spawn -- and why the same call covers both.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let dir = claude_config_dir(home, "second");
        touch_dir(&dir);

        assert_eq!(ensure_folder_trusted(home, "second", &root, Some(&parent)), Ok(true));
        assert_eq!(recorded_trust(&dir.join(".claude.json"), &root), Some(true));
        assert!(!home.join(".claude.json").exists(), "the default account's file is untouched");
    }

    #[test]
    fn the_default_accounts_trust_file_sits_beside_its_config_dir_not_inside_it() {
        // Checked against a real installation: with CLAUDE_CONFIG_DIR unset
        // the trust record is `<home>/.claude.json`. `<home>/.claude/.claude.json`
        // also exists there but carries no `projects` table at all, so
        // joining the config dir would write a file claude never reads.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert_eq!(claude_trust_file(home, ""), home.join(".claude.json"));
        assert_eq!(claude_trust_file(home, "default"), home.join(".claude.json"));
        assert_eq!(
            claude_trust_file(home, "second"),
            home.join(CLAUDE_ACCOUNTS_DIR).join("second").join(".claude.json")
        );
    }

    #[test]
    fn a_relative_declared_prefix_is_refused_and_trusts_nothing() {
        // A relative prefix cannot be reasoned about against an absolute
        // root: say so instead of silently trusting or silently skipping.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let root = declared_parent(home).join("some-repo");
        touch_dir(&root);

        let err = ensure_folder_trusted(home, "", &root, Some(Path::new("declared-parent"))).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
        assert!(!claude_trust_file(home, "").exists());
    }

    #[test]
    fn an_unreadable_trust_file_is_refused_rather_than_clobbered() {
        // Claude Code owns this file. A shape we cannot parse is left alone:
        // the caller logs and starts the agent anyway, so the worst outcome
        // is the prompt appearing.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        let before = b"not json at all".to_vec();
        std::fs::write(&file, &before).unwrap();

        assert!(ensure_folder_trusted(home, "", &root, Some(&parent)).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    #[test]
    fn an_already_trusted_root_is_left_byte_identical() {
        // Every spawn calls this; a root claude already trusts must not mean
        // rewriting a file claude may be writing at the same moment.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let before = std::fs::read(&file).unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(false));
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    /// A stand-in for what the installer writes into this box's own
    /// settings file -- built in a tempdir, so no real path from any
    /// machine appears in this repo.
    fn declare_in_settings(config_dir: &Path, prefix: &Path) {
        touch_dir(config_dir);
        std::fs::write(
            config_dir.join(SETTINGS_FILE),
            toml::to_string(&serde_json::json!({"layout": {"preset": "auto"}, "trust": {"root_prefix": prefix.to_string_lossy()}})).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn the_declared_prefix_is_read_from_this_boxs_own_settings_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let parent = declared_parent(tmp.path());
        declare_in_settings(&config, &parent);
        assert_eq!(declared_root_prefix(&config), Some(parent));
    }

    #[test]
    fn an_absent_or_silent_settings_file_declares_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        assert_eq!(declared_root_prefix(&config), None, "no file -- nothing is trusted unless declared");
        touch_dir(&config);
        std::fs::write(config.join(SETTINGS_FILE), "[layout]\npreset = \"auto\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "somebody else's keys declare nothing");
        std::fs::write(config.join(SETTINGS_FILE), "[trust]\nroot_prefix = \"\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "an empty declaration is not a blanket one");
        std::fs::write(config.join(SETTINGS_FILE), "# root_prefix = \"/x\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "a commented line is not a declaration");
    }

    #[test]
    fn the_same_key_outside_the_trust_table_is_not_a_declaration() {
        assert_eq!(
            parse_declared_root_prefix("[layout]\nroot_prefix = \"/somebody/elses/key\"\n"),
            None
        );
    }

    // TOML table whitespace does not change the declaration schema.
    #[test]
    fn a_spaced_trust_header_is_the_same_table() {
        assert_eq!(
            parse_declared_root_prefix("[ trust ]\nroot_prefix = \"/declared/here\"\n"),
            Some(PathBuf::from("/declared/here"))
        );
    }
}
