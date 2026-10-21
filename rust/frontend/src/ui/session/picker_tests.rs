use super::*;

// Workspace-create picker root: a Windows FE with no `$HOME` in its
// process env must not seed the picker (and hence `workspace.create`'s
// `project_root`) with a driveless `/` — see `picker_local_home_fallback`.

/// Field defect (2026-09-05): on a Windows box, "+ create new" under
/// the LOCAL host started the picker at the remote backend's Linux home.
#[test]
fn local_host_ignores_a_configured_root_that_does_not_exist_here() {
    let start = picker_start_for_host(
        "local",
        Some(r"C:\Users\u"),
        Some("/home/u/dev"),
        Some("/home/u"),
        "C:/fe-home".to_string(),
        |_| false,
    );
    assert_eq!(start, r"C:\Users\u");
}

#[test]
fn local_host_honours_a_configured_root_that_exists_here() {
    let start = picker_start_for_host(
        "local",
        Some(r"C:\Users\u"),
        Some(r"C:\Users\u\dev"),
        None,
        "C:/fe-home".to_string(),
        |p| p.ends_with("dev"),
    );
    assert_eq!(start, r"C:\Users\u\dev");
}

#[test]
fn local_host_falls_back_to_the_frontends_home_without_a_default_row() {
    let start = picker_start_for_host("local", None, None, Some("/home/u"), "C:/fe-home".to_string(), |_| false);
    assert_eq!(start, "C:/fe-home");
}

#[test]
fn remote_host_keeps_configured_then_remote_home_then_default_row() {
    let cfg = picker_start_for_host("host-4", Some("/home/u"), Some("/home/u/dev"), Some("/home/u"), "C:/fe".into(), |_| false);
    assert_eq!(cfg, "/home/u/dev");
    let home = picker_start_for_host("host-4", Some("/home/u"), None, Some("/home/remote"), "C:/fe".into(), |_| false);
    assert_eq!(home, "/home/remote");
    let row = picker_start_for_host("host-4", Some("/home/u"), None, None, "C:/fe".into(), |_| false);
    assert_eq!(row, "/home/u");
    let last = picker_start_for_host("host-4", None, None, None, "C:/fe".into(), |_| false);
    assert_eq!(last, "C:/fe");
}

#[test]
fn picker_local_home_fallback_prefers_env_home() {
    assert_eq!(
        picker_local_home_fallback(
            Some("/home/u/explicit".to_string()),
            Some(PathBuf::from(r"C:\Users\u"))
        ),
        "/home/u/explicit"
    );
}

#[test]
fn picker_local_home_fallback_uses_os_home_over_bare_root() {
    // This is the regression: before the fix, an absent `$HOME` fell
    // straight to "/" even when the OS could report a real home dir.
    assert_eq!(
        picker_local_home_fallback(None, Some(PathBuf::from("/home/u"))),
        "/home/u"
    );
}

#[test]
fn picker_local_home_fallback_falls_back_to_root_when_nothing_resolves() {
    assert_eq!(picker_local_home_fallback(None, None), "/");
}

#[test]
#[cfg(windows)]
fn picker_local_home_fallback_keeps_drive_letter_backslash_form() {
    assert_eq!(
        picker_local_home_fallback(None, Some(PathBuf::from(r"C:\Users\u\HomeLab\r"))),
        r"C:\Users\u\HomeLab\r"
    );
}

#[test]
#[cfg(windows)]
fn picker_local_home_fallback_keeps_drive_letter_forward_slash_form() {
    assert_eq!(
        picker_local_home_fallback(None, Some(PathBuf::from("C:/Users/u/HomeLab/r"))),
        "C:/Users/u/HomeLab/r"
    );
}

fn account(name: &str, kinds: &[&str], logged_in: &[(&str, bool)]) -> crate::net::transport::AccountInfo {
    crate::net::transport::AccountInfo {
        name: name.to_string(),
        kinds: kinds.iter().map(|s| s.to_string()).collect(),
        logged_in: logged_in.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
    }
}

fn picker_with_accounts(accounts: Vec<crate::net::transport::AccountInfo>) -> WorkspacePicker {
    WorkspacePicker {
        host: "local".to_string(),
        show_hidden: true,
        current_path: "/tmp".to_string(),
        entries: Vec::new(),
        selected: 0,
        reveal: None,
        accounts,
        account_selected: 0,
    }
}

fn picker_dir(path: &str) -> crate::net::transport::DirEntry {
    crate::net::transport::DirEntry {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        has_children: true,
    }
}

/// Going back up with Left returns you to where you went in: the
/// directory just left is an entry of the parent, and the cursor lands
/// on it instead of the top.
#[test]
fn picker_listing_lands_the_cursor_on_the_directory_left_behind() {
    let mut p = picker_with_accounts(Vec::new());
    p.reveal = Some("c".to_string());
    p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b"), picker_dir("/tmp/c")]);
    assert_eq!(p.selected, 2);
    assert_eq!(p.reveal, None, "consumed by the listing that answered");
}

/// The remembered entry is not in the listing (a dot-directory, with
/// hidden folders now off): the top, never a stale index.
#[test]
fn picker_listing_falls_back_to_the_top_when_the_entry_is_gone() {
    let mut p = picker_with_accounts(Vec::new());
    p.reveal = Some(".hidden".to_string());
    p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b")]);
    assert_eq!(p.selected, 0);
}

/// Nothing remembered: an in-range cursor stays put, and one the
/// listing no longer reaches goes to the top.
#[test]
fn picker_listing_keeps_an_in_range_cursor_and_clamps_the_rest() {
    let mut p = picker_with_accounts(Vec::new());
    p.selected = 1;
    p.land_listing(vec![picker_dir("/tmp/a"), picker_dir("/tmp/b")]);
    assert_eq!(p.selected, 1);
    p.selected = 5;
    p.land_listing(vec![picker_dir("/tmp/a")]);
    assert_eq!(p.selected, 0);
}

/// The new-session prompt's account list handling: default-only hides the field.
#[test]
fn account_choice_hidden_when_only_default() {
    let p = picker_with_accounts(vec![account("default", &["claude"], &[("claude", true)])]);
    assert!(!p.account_choice_visible());
}

/// An old daemon with no `accounts.list` handler (or one whose reply
/// fails to parse) surfaces as an empty accounts list — same hidden
/// treatment as default-only, no error.
#[test]
fn account_choice_hidden_when_daemon_never_answered() {
    let p = picker_with_accounts(Vec::new());
    assert!(!p.account_choice_visible());
}

#[test]
fn account_choice_visible_with_a_second_declared_account() {
    let p = picker_with_accounts(vec![
        account("default", &["claude"], &[("claude", true)]),
        account("team", &["claude"], &[("claude", true)]),
    ]);
    assert!(p.account_choice_visible());
}

/// Not-logged-in entries are marked, but still selectable — a
/// never-logged-in folder is a NORMAL choice (decided): the
/// row's own pane runs the login on first start.
#[test]
fn account_not_logged_in_is_marked_and_still_selectable() {
    let logged_out = account("team", &["claude"], &[("claude", false)]);
    assert!(!logged_out.any_logged_in());
    let logged_in = account("default", &["claude"], &[("claude", true)]);
    assert!(logged_in.any_logged_in());
    // Cycling never skips a not-logged-in entry — it's a full member
    // of the rotation, just annotated.
    let mut p = picker_with_accounts(vec![logged_in, logged_out]);
    assert_eq!(p.account_selected, 0);
    p.account_selected = (p.account_selected + 1) % p.accounts.len();
    assert_eq!(p.account_selected, 1);
    assert!(!p.accounts[p.account_selected].any_logged_in());
}

#[test]
fn selected_account_is_chosen_by_name_not_position() {
    let chosen = |names: &[&str], selected: usize| {
        let mut p = picker_with_accounts(names.iter().map(|n| account(n, &[], &[])).collect());
        p.account_selected = selected;
        selected_account(&p)
    };
    assert_eq!(chosen(&["team", "other"], 0).as_deref(), Some("team"));
    assert_eq!(chosen(&["default", "team"], 0), None);
    assert_eq!(chosen(&["default", "team"], 1).as_deref(), Some("team"));
    assert_eq!(chosen(&["team", "default"], 1), None);
    assert_eq!(chosen(&[], 0), None);
    assert_eq!(chosen(&["default"], 9), None);
}
