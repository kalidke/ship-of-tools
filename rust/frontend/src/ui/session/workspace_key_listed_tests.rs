use super::*;
use crate::ui::ws_info;

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
    let mut directory = RowDirectory::default();
    for (host, rows) in [
        (a.clone(), vec![a_row, a_default]),
        (b.clone(), vec![b_row, b_default]),
    ] {
        let update = directory.prepare_replace(host, rows);
        directory.commit_replace(update);
    }
    let by_slug = directory.resolve(&b, "project").unwrap();
    let by_id = directory.resolve(&b, "id-b").unwrap();
    assert_eq!(by_slug.identity(), by_id.identity());
    assert_eq!(by_slug.row_key(), &(b.clone(), "project".into()));
    assert_eq!(by_slug.attachment().unwrap(), Some("stored-b".into()));
    for (host, id, attachment, default_slug, default_attachment) in [
        (&a, "id-a", "stored-a", "base-a", "default-a"),
        (&b, "id-b", "stored-b", "base-b", "default-b"),
    ] {
        for spelling in ["project", id] {
            let target = directory.resolve(host, spelling).unwrap();
            assert_eq!(
                target.identity(),
                &ResultRowIdentity {
                    host: host.clone(),
                    workspace_id: id.into()
                }
            );
            assert_eq!(target.row_key(), &(host.clone(), "project".into()));
            assert_eq!(target.view_key(), target.row_key());
            assert_eq!(target.slug().as_deref(), Some("project"));
            assert_eq!(target.attachment().unwrap().as_deref(), Some(attachment));
        }
        for spelling in [
            "",
            "default",
            "<default>",
            default_slug,
            &format!("ws-{default_slug}-0000"),
        ] {
            let target = directory.resolve(host, spelling).unwrap();
            assert_eq!(target.row_key(), &(host.clone(), default_slug.into()));
            assert_eq!(target.view_key(), &(host.clone(), "<default>".into()));
            assert_eq!(
                target.attachment().unwrap().as_deref(),
                Some(default_attachment)
            );
            assert!(target.slug().is_none());
        }
    }
    assert!(directory.resolve(&a, "id-b").is_err());
    assert!(directory.resolve(&b, "id-a").is_err());
    assert_ne!(
        directory.resolve(&a, "project").unwrap().identity(),
        by_id.identity()
    );
}

#[test]
fn ambiguous_empty_and_unknown_targets_are_refused() {
    let host = "<host>".to_string();
    let row = ws_info("project", "stored");
    let mut directory = RowDirectory::default();
    assert!(
        directory.resolve(&host, "project").is_err(),
        "no listing yet"
    );
    let update = directory.prepare_replace(host.clone(), vec![row.clone(), row.clone()]);
    directory.commit_replace(update);
    assert!(directory.resolve(&host, "project").is_err());
    let mut duplicate_id = row.clone();
    duplicate_id.slug = "other".into();
    let update = directory.prepare_replace(host.clone(), vec![row.clone(), duplicate_id]);
    directory.commit_replace(update);
    assert!(
        directory.resolve(&host, "project").is_err(),
        "a unique slug with a duplicated canonical id"
    );
    let mut conflicting_spelling = ws_info("other", "other-target");
    conflicting_spelling.workspace_id = "project".into();
    let update = directory.prepare_replace(host.clone(), vec![row.clone(), conflicting_spelling]);
    directory.commit_replace(update);
    assert!(
        directory.resolve(&host, "project").is_err(),
        "one row's slug is another row's id"
    );
    let mut empty = row.clone();
    empty.workspace_id.clear();
    let update = directory.prepare_replace(host.clone(), vec![empty]);
    directory.commit_replace(update);
    assert!(directory.resolve(&host, "project").is_err());
    let mut no_target = row;
    no_target.session_name.clear();
    let update = directory.prepare_replace(host.clone(), vec![no_target.clone()]);
    directory.commit_replace(update);
    assert!(directory.resolve(&host, "unknown").is_err());
    assert!(directory
        .resolve(&host, "project")
        .unwrap()
        .attachment()
        .is_err());
    no_target.is_default = true;
    no_target.agent = "none".into();
    let update = directory.prepare_replace(host.clone(), vec![no_target]);
    directory.commit_replace(update);
    let anchor = directory.resolve(&host, "default").unwrap();
    assert!(!anchor.visible());
    assert_eq!(anchor.attachment().unwrap(), None);
}
