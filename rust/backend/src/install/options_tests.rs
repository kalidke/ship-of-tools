use super::*;

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn refusal(s: &str) -> String {
    let stop = parse(&argv(s)).unwrap_err();
    assert_eq!((stop.check.as_str(), stop.code), ("options", 2), "{s}: {stop:?}");
    stop.detail
}

/// The option names never move: every Unix option of the entry parses, with --pack the bootstrap adds.
#[cfg(unix)]
#[test]
fn the_unix_options_keep_their_names() {
    let o = parse(&argv("--pack /w --backend box --version v1.2.3 --prefix /p --hub hub --no-service --force-role-change --restart-daemon")).unwrap();
    assert_eq!(o.pack, PathBuf::from("/w"));
    assert_eq!(o.role, Some(RoleFlag::Remote("box".into())));
    assert_eq!((o.version.as_deref(), o.prefix.as_deref(), o.hub.as_deref()), (Some("v1.2.3"), Some(std::path::Path::new("/p")), Some("hub")));
    assert!(o.no_service && o.force_role_change && o.restart_daemon && o.named());
    assert_eq!(parse(&argv("--pack /w --local")).unwrap().role, Some(RoleFlag::Local));
    assert_eq!(parse(&argv("--pack /w --be-only --archive /a")).unwrap().role, Some(RoleFlag::BeOnly));
    assert!(!parse(&argv("--pack /w")).unwrap().named());
}

/// An option error is a refusal in the one verdict line: an unknown option, a missing value, both targets, no pack.
#[cfg(unix)]
#[test]
fn an_option_error_is_a_refusal() {
    assert_eq!(refusal("--pack /w --frob"), "unknown flag: --frob");
    assert_eq!(refusal("--pack /w --version"), "--version needs a value");
    assert_eq!(refusal("--pack /w --backend"), "--backend needs an ssh alias");
    assert!(refusal("--pack /w --version v1 --archive /a").contains("exclusive"));
    assert!(refusal("--local").starts_with("no --pack"));
}

#[cfg(windows)]
#[test]
fn the_windows_options_keep_their_names_in_any_case() {
    let o = parse(&argv("--pack C:\\w -version v1.2.3 -Prefix C:\\p -HUB hub -RestartDaemon")).unwrap();
    assert_eq!((o.version.as_deref(), o.hub.as_deref(), o.restart_daemon), (Some("v1.2.3"), Some("hub"), true));
    assert_eq!(refusal("--pack C:\\w -Frob"), "unknown option: -Frob");
    assert_eq!(refusal("--pack C:\\w -Version -Hub"), "-Version needs a value");
    assert!(refusal("--pack C:\\w -Version v1 -Archive C:\\a").contains("exclusive"));
}
