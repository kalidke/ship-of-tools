//! Scope, project-key spelling and JSON preservation controls.
use super::super::accounts::CLAUDE_ACCOUNTS_DIR;
use super::super::support_tests::touch_dir;
use super::*;

fn claude_trust_file(home: &Path, account: &str) -> PathBuf {
    if account.is_empty() || account == "default" {
        home.join(CLAUDE_TRUST_FILE)
    } else {
        super::super::accounts::claude_config_dir(home, account).join(CLAUDE_TRUST_FILE)
    }
}
fn ensure_folder_trusted(
    home: &Path,
    account: &str,
    root: &Path,
    prefix: Option<&Path>,
) -> Result<TrustOutcome, String> {
    super::ensure_folder_trusted(&claude_trust_file(home, account), root, prefix)
}

#[test]
fn claude_project_key_maps_backslashes_only_on_windows() {
    assert_eq!(
        claude_project_key(r"C:\Users\u\repo", true),
        "C:/Users/u/repo"
    );
    assert_eq!(
        claude_project_key(r"C:\Users\u\repo", false),
        r"C:\Users\u\repo"
    );
    assert_eq!(
        claude_project_key("C:/Users/u/repo", true),
        "C:/Users/u/repo"
    );
    assert_eq!(
        claude_project_key("C:/Users/u/repo", false),
        "C:/Users/u/repo"
    );
}

#[test]
#[cfg(windows)]
fn a_backslash_root_is_recorded_under_the_slash_key() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let parent = declared_parent(home);
    let root = parent.join("some-repo");
    touch_dir(&root);
    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    let text = std::fs::read_to_string(claude_trust_file(home, "")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let key = claude_project_key(&root.to_string_lossy(), true);
    assert!(!key.contains('\\'));
    assert_eq!(
        doc["projects"][&key][TRUST_ACCEPTED_KEY],
        serde_json::Value::Bool(true)
    );
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

    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    assert_eq!(
        recorded_trust(&claude_trust_file(home, ""), &root),
        Some(true)
    );
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

    assert_eq!(
        ensure_folder_trusted(home, "", &outside, Some(&parent)),
        Ok(TrustOutcome::Outside)
    );
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

    assert_eq!(
        ensure_folder_trusted(home, "", &sibling, Some(&parent)),
        Ok(TrustOutcome::Outside)
    );
    assert!(
        !claude_trust_file(home, "").exists(),
        "nothing may be written"
    );
}

#[test]
fn a_dot_dot_spelling_that_escapes_the_prefix_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let parent = declared_parent(home);
    let escaped = home.join("elsewhere").join("some-repo");
    touch_dir(&escaped);
    let spelled = parent.join("..").join("elsewhere").join("some-repo");
    assert_eq!(
        ensure_folder_trusted(home, "", &spelled, Some(&parent)),
        Ok(TrustOutcome::Outside)
    );
    assert!(!claude_trust_file(home, "").exists());
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

    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(v["numStartups"], 7);
    assert_eq!(v["installMethod"], "native");
    assert_eq!(
        v["projects"][other]["hasTrustDialogAccepted"], false,
        "another project's own answer stands"
    );
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

    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        v.as_object().unwrap().len(),
        1,
        "no key invented beyond projects"
    );
    assert_eq!(v["projects"].as_object().unwrap().len(), 1);
    assert_eq!(recorded_trust(&file, &root), Some(true));
}

#[test]
fn no_declared_prefix_writes_nothing_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let root = declared_parent(home).join("some-repo");
    touch_dir(&root);

    assert_eq!(
        ensure_folder_trusted(home, "", &root, None),
        Ok(TrustOutcome::NotDeclared)
    );
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
    let dir = super::super::accounts::claude_config_dir(home, "second");
    touch_dir(&dir);

    assert_eq!(
        ensure_folder_trusted(home, "second", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    assert_eq!(recorded_trust(&dir.join(".claude.json"), &root), Some(true));
    assert!(
        !home.join(".claude.json").exists(),
        "the default account's file is untouched"
    );
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
    assert_eq!(
        claude_trust_file(home, "default"),
        home.join(".claude.json")
    );
    assert_eq!(
        claude_trust_file(home, "second"),
        home.join(CLAUDE_ACCOUNTS_DIR)
            .join("second")
            .join(".claude.json")
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

    let err =
        ensure_folder_trusted(home, "", &root, Some(Path::new("declared-parent"))).unwrap_err();
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
    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::Recorded)
    );
    let before = std::fs::read(&file).unwrap();

    assert_eq!(
        ensure_folder_trusted(home, "", &root, Some(&parent)),
        Ok(TrustOutcome::AlreadyTrusted)
    );
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
    assert_eq!(
        declared_root_prefix(&config),
        None,
        "no file -- nothing is trusted unless declared"
    );
    touch_dir(&config);
    std::fs::write(config.join(SETTINGS_FILE), "[layout]\npreset = \"auto\"\n").unwrap();
    assert_eq!(
        declared_root_prefix(&config),
        None,
        "somebody else's keys declare nothing"
    );
    std::fs::write(config.join(SETTINGS_FILE), "[trust]\nroot_prefix = \"\"\n").unwrap();
    assert_eq!(
        declared_root_prefix(&config),
        None,
        "an empty declaration is not a blanket one"
    );
    std::fs::write(config.join(SETTINGS_FILE), "# root_prefix = \"/x\"\n").unwrap();
    assert_eq!(
        declared_root_prefix(&config),
        None,
        "a commented line is not a declaration"
    );
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

#[test]
fn resolution_failure_preserves_existing_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let prefix = declared_parent(home);
    let file = claude_trust_file(home, "");
    let before = b"{\"projects\":{},\"unrelated\":true}";
    std::fs::write(&file, before).unwrap();
    let error =
        ensure_folder_trusted(home, "", &prefix.join("missing"), Some(&prefix)).unwrap_err();
    assert!(error.contains("project root"));
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn resolved_aliases_keep_the_original_cwd_key() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let prefix = declared_parent(home);
    let root = prefix.join("repo");
    touch_dir(&root);
    let alias = home.join("alias");
    std::os::unix::fs::symlink(&prefix, &alias).unwrap();
    let cwd = alias.join("repo");
    assert_eq!(
        ensure_folder_trusted(home, "", &cwd, Some(&prefix)),
        Ok(TrustOutcome::Recorded)
    );
    let file = claude_trust_file(home, "");
    assert_eq!(recorded_trust(&file, &cwd), Some(true));
    assert_eq!(recorded_trust(&file, &root), None);
    println!("W1 C2 alias PASS: OS-resolved containment; original cwd key retained");
}

#[cfg(windows)]
#[test]
fn windows_case_and_drive_alias_keep_the_cwd_key() {
    let temp = tempfile::tempdir().unwrap();
    let home = crate::agents::support_tests::platform_spelling(temp.path());
    let prefix = declared_parent(&home);
    let root = prefix.join("mixed-case");
    touch_dir(&root);
    let mut spelling = root.to_str().unwrap().to_owned();
    if spelling.as_bytes().get(1) == Some(&b':') {
        spelling.replace_range(..1, &spelling[..1].to_ascii_lowercase());
    } else {
        println!("W1 P4 drive-alias NOT CHECKED: fixture has no drive-letter spelling");
    }
    let alias = PathBuf::from(spelling).parent().unwrap().join("MIXED-CASE");
    assert_eq!(
        crate::agents::support_tests::platform_spelling(&alias),
        crate::agents::support_tests::platform_spelling(&root),
        "W1 P4 STOP: case/drive aliases do not converge"
    );
    assert_eq!(
        ensure_folder_trusted(&home, "", &alias, Some(&prefix)),
        Ok(TrustOutcome::Recorded)
    );
    assert_eq!(
        recorded_trust(&claude_trust_file(&home, ""), &alias),
        Some(true)
    );
    println!("W1 P4 case/drive spelling PASS: OS comparison converges; original cwd key retained");
}

#[cfg(windows)]
#[test]
fn windows_junction_escape_records_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let home = crate::agents::support_tests::platform_spelling(temp.path());
    let prefix = declared_parent(&home);
    let outside = home.join("outside");
    touch_dir(&outside);
    let link = prefix.join("escape");
    let mut command = std::process::Command::new("cmd.exe");
    command
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[allow(
        clippy::disallowed_methods,
        reason = "test-only junction facility; retained child is drained within its bound"
    )]
    let child = command.spawn().unwrap();
    let (status, _, _) =
        sot_log::test_isolated::drain(child).wait_within(std::time::Duration::from_secs(20));
    if !status.success() {
        println!("W1 P4 junction NOT CHECKED: runner cannot create the fixture junction");
        return;
    }
    assert_eq!(
        crate::agents::support_tests::platform_spelling(&link),
        crate::agents::support_tests::platform_spelling(&outside),
        "W1 P4 STOP: junction identity does not converge"
    );
    assert_eq!(
        ensure_folder_trusted(&home, "", &link, Some(&prefix)),
        Ok(TrustOutcome::Outside)
    );
    assert!(
        !claude_trust_file(&home, "").exists(),
        "W1 scope violation: junction escape wrote a trust file"
    );
    std::fs::remove_dir(&link).unwrap();
    println!("W1 P4 junction PASS: OS comparison converges; escape records nothing");
}

#[cfg(windows)]
#[test]
fn windows_short_name_keeps_the_cwd_key_when_available() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let temp = tempfile::tempdir().unwrap();
    let home = crate::agents::support_tests::platform_spelling(temp.path());
    let prefix = declared_parent(&home);
    let root = prefix.join("long directory spelling for short alias");
    touch_dir(&root);
    let input: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
            input.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        println!("W1 P4 short-name NOT CHECKED: runner exposes no short spelling");
        return;
    }
    let alias = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    if alias == root {
        println!("W1 P4 short-name NOT CHECKED: filesystem supplies no distinct short spelling");
        return;
    }
    assert_eq!(
        crate::agents::support_tests::platform_spelling(&alias),
        crate::agents::support_tests::platform_spelling(&root),
        "W1 P4 STOP: short-name identity does not converge"
    );
    assert_eq!(
        ensure_folder_trusted(&home, "", &alias, Some(&prefix)),
        Ok(TrustOutcome::Recorded)
    );
    assert_eq!(
        recorded_trust(&claude_trust_file(&home, ""), &alias),
        Some(true)
    );
    println!("W1 P4 short-name PASS: OS comparison converges; original cwd key retained");
}
