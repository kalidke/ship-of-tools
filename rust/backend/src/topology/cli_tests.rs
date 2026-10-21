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
