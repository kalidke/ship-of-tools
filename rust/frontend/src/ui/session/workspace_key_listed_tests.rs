use super::*;

#[test]
fn listed_resolution_keeps_each_host_and_key_space() {
    let a = "<host-a>".to_string();
    let b = "<host-b>".to_string();
    let mut a_row = ws_info("project", "stored-a");
    a_row.workspace_id = "id-a".into();
    let mut b_row = ws_info("project", "stored-b");
    b_row.workspace_id = "id-b".into();
    let mut a_default = ws_info("base-a", "default-a");
    a_default.is_default = true;
    let mut b_default = ws_info("base-b", "default-b");
    b_default.is_default = true;
    let lists = HashMap::from([
        (a.clone(), vec![a_row, a_default]),
        (b.clone(), vec![b_row, b_default]),
    ]);
    let by_slug = resolve_listed_workspace(&lists, &b, "project").unwrap();
    let by_id = resolve_listed_workspace(&lists, &b, "id-b").unwrap();
    assert_eq!(by_slug.identity(), by_id.identity());
    assert_eq!(by_slug.row_key(), &(b.clone(), "project".into()));
    assert_eq!(by_slug.attachment().unwrap(), Some("stored-b".into()));
    for spelling in ["", "default", "<default>", "base-b", "ws-base-b-0000"] {
        let target = resolve_listed_workspace(&lists, &b, spelling).unwrap();
        assert_eq!(target.row_key(), &(b.clone(), "base-b".into()));
        assert_eq!(target.view_key(), &(b.clone(), "<default>".into()));
        assert!(target.slug().is_none());
    }
    assert!(resolve_listed_workspace(&lists, &a, "id-b").is_err());
    assert_ne!(
        resolve_listed_workspace(&lists, &a, "project")
            .unwrap()
            .identity(),
        by_id.identity()
    );
}

#[test]
fn ambiguous_empty_and_unknown_targets_are_refused() {
    let host = "<host>".to_string();
    let row = ws_info("project", "stored");
    let mut lists = HashMap::from([(host.clone(), vec![row.clone(), row.clone()])]);
    assert!(resolve_listed_workspace(&lists, &host, "project").is_err());
    let mut empty = row.clone();
    empty.workspace_id.clear();
    lists.insert(host.clone(), vec![empty]);
    assert!(resolve_listed_workspace(&lists, &host, "project").is_err());
    let mut no_target = row;
    no_target.session_name.clear();
    lists.insert(host.clone(), vec![no_target.clone()]);
    assert!(resolve_listed_workspace(&lists, &host, "unknown").is_err());
    assert!(resolve_listed_workspace(&lists, &host, "project")
        .unwrap()
        .attachment()
        .is_err());
    no_target.is_default = true;
    no_target.agent = "none".into();
    lists.insert(host.clone(), vec![no_target]);
    let anchor = resolve_listed_workspace(&lists, &host, "default").unwrap();
    assert!(!anchor.visible());
    assert_eq!(anchor.attachment().unwrap(), None);
}
