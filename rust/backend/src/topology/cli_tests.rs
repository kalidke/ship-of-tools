use super::*;

#[test]
fn dry_run_is_default_yes_flips_it() {
    let dry = |args: &[&str]| !args.iter().any(|a| *a == "--yes");
    assert!(dry(&[]));
    assert!(dry(&["--dry-run"]));
    assert!(!dry(&["--yes"]));
}

fn w(words: &[&str]) -> Vec<String> {
    words.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parse_edit_covers_every_shape() {
    assert_eq!(
        parse_edit(&w(&["add", "gamma", "--daemon"])).unwrap(),
        topology::TopologyEdit::AddHost { name: "gamma".into(), daemon: true, frontend: false }
    );
    assert_eq!(parse_edit(&w(&["remove", "gamma"])).unwrap(), topology::TopologyEdit::RemoveHost { name: "gamma".into() });
    assert_eq!(
        parse_edit(&w(&["flag", "gamma", "daemon", "true"])).unwrap(),
        topology::TopologyEdit::SetFlag { name: "gamma".into(), key: "daemon".into(), value: true }
    );
    assert_eq!(
        parse_edit(&w(&["monitor-add", "gpu", "box"])).unwrap(),
        topology::TopologyEdit::MonitorAdd { label: "gpu".into(), target: "box".into() }
    );
    assert_eq!(parse_edit(&w(&["monitor-remove", "gpu"])).unwrap(), topology::TopologyEdit::MonitorRemove { label: "gpu".into() });
    assert!(parse_edit(&w(&["flag", "gamma", "daemon", "maybe"])).is_err());
    assert!(parse_edit(&w(&[])).is_err());
}

#[test]
fn hub_endpoint_is_local_on_the_hub_else_an_ssh_child_into_it() {
    let t = topology::parse("hub = \"alpha\"\n[host.alpha]\ndaemon = true\n").unwrap();
    assert_eq!(hub_endpoint(&t, "alpha"), topology::endpoint::local_endpoint());
    assert_eq!(hub_endpoint(&t, "beta"), "ssh:alpha");
}

#[test]
fn sync_refuses_a_fetched_file_that_does_not_list_self() {
    let fetched = topology::parse("hub = \"hub\"\n[host.hub]\ndaemon = true\n").unwrap();
    let e = refuse_if_not_listed(&fetched, "frontend-box").unwrap_err();
    assert!(e.contains("does not list this box") && e.contains("[host.frontend-box]"), "{e}");
    assert!(refuse_if_not_listed(&fetched, "hub").is_ok());
}

#[test]
fn self_is_hub_predicate() {
    assert!(self_is_hub("hub-box", "hub-box"));
    assert!(!self_is_hub("frontend-box", "hub-box"));
}

#[test]
fn self_is_hub_is_exact_byte_equality() {
    assert!(!self_is_hub("Hub-Box", "hub-box"));
    assert!(!self_is_hub("hub-box ", "hub-box"));
    assert!(self_is_hub("", ""));
}

#[test]
fn resolve_hub_prefers_the_local_copy_over_a_disagreeing_flag() {
    let t = topology::parse("hub = \"hub-a\"\n[host.hub-a]\ndaemon = true\n").unwrap();
    let dest = std::path::Path::new("/nowhere/hosts.toml");
    assert_eq!(resolve_hub(Some(&t), None, dest).unwrap(), "hub-a", "no flag: the copy's hub");
    assert_eq!(resolve_hub(Some(&t), Some("hub-a"), dest).unwrap(), "hub-a", "agreeing flag: no complaint, same answer");
    assert_eq!(resolve_hub(Some(&t), Some("hub-b"), dest).unwrap(), "hub-a", "disagreeing flag: the copy still wins");
}

#[test]
fn resolve_hub_falls_back_to_the_flag_only_with_no_local_copy() {
    let dest = std::path::Path::new("/nowhere/hosts.toml");
    assert_eq!(resolve_hub(None, Some("hub-a"), dest).unwrap(), "hub-a");
    let e = resolve_hub(None, None, dest).unwrap_err();
    assert!(e.contains("/nowhere/hosts.toml") && e.contains("pass --hub <alias>"), "{e}");
}

#[test]
fn sync_refuses_a_hub_that_is_not_a_plain_host_name() {
    // `--hub` becomes an argument of ssh, which reads a leading `-` as an option.
    let dest = std::path::Path::new("/nowhere/hosts.toml");
    let e = resolve_hub(None, Some("-oProxyCommand=touch-x"), dest).unwrap_err();
    assert!(e.contains("not a plain host name"), "{e}");
    assert_eq!(resolve_hub(None, Some("hub-box.example"), dest).unwrap(), "hub-box.example");
}

/// On a shared home the hub's daemon writes the same file `sync` replaces. A copy that changed while `sync` fetched is a
/// newer version than the one fetched, and it survives.
#[test]
fn sync_replaces_only_the_version_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("hosts.toml");
    let read = "hub = \"a\"\n[host.a]\ndaemon = true\n";
    let newer = "hub = \"a\"\n[host.a]\ndaemon = true\n[host.b]\n";
    let fetched = "hub = \"a\"\n[host.a]\ndaemon = true\n[host.c]\n";
    std::fs::write(&dest, read).unwrap();
    std::fs::write(&dest, newer).unwrap();
    let e = publish_synced(&dest, Some(read), fetched).unwrap_err();
    assert!(e.contains("changed while"), "{e}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), newer, "the newer edit survives");
    // A file that appeared after a read of no file is just as new.
    let e = publish_synced(&dest, None, fetched).unwrap_err();
    assert!(e.contains("changed while"), "{e}");
    // The version that was read is replaced.
    publish_synced(&dest, Some(newer), fetched).unwrap();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), fetched);
}

/// A verb's refusal has one reader, `refusal`, and it reads back the line `report` prints (exit 2), telling the one
/// for a missing hosts.toml from any other; any other exit or text is no refusal.
#[test]
fn refusal_reads_back_what_report_prints() {
    let missing = format!("{NO_HOSTS}/h/.config/sot/hosts.toml (run `sotd topology sync --hub <alias>`)");
    assert_eq!(refusal(2, &format!("{}\n", refusal_line(&missing))), Some((missing.clone(), true)));
    assert_eq!(refusal(2, &format!("{}\n", refusal_line("line 3: unknown key `x`"))), Some(("line 3: unknown key `x`".to_string(), false)));
    for (code, err) in [(1, format!("{}\n", refusal_line(&missing))), (2, "another program's line\n".to_string()), (2, format!("{}\n", refusal_line("  ")))] {
        assert_eq!(refusal(code, &err), None, "exit {code} {err:?}");
    }
}
