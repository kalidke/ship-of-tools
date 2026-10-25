use super::*;
use crate::ui::ws_info;

#[test]
fn union_replace_leaves_the_other_hosts_list_intact() {
    let a = "alpha".to_string();
    let b = "beta".to_string();
    let mut directory = RowDirectory::default();
    // Equal slugs and legacy ids, with deliberately different stored attachment names.
    for (host, target) in [(&a, "first-target"), (&b, "second-target")] {
        let mut row = ws_info("shared", target);
        row.workspace_id = "legacy".into();
        let update = directory.prepare_replace(host.clone(), vec![row]);
        assert!(update.retired().is_empty());
        directory.commit_replace(update);
    }
    let mut successor = ws_info("shared", "replacement-target");
    successor.workspace_id = "successor".into();
    let update = directory.prepare_replace(a.clone(), vec![successor]);
    assert_eq!(
        directory
            .resolve(&a, "legacy")
            .unwrap()
            .attachment()
            .unwrap()
            .as_deref(),
        Some("first-target")
    );
    directory.commit_replace(update);

    assert_eq!(directory.rows(&a).unwrap().len(), 1);
    assert_eq!(directory.rows(&b).unwrap().len(), 1);
    assert_eq!(
        directory
            .resolve(&a, "shared")
            .unwrap()
            .identity()
            .workspace_id,
        "successor"
    );
    assert_eq!(
        directory
            .resolve(&b, "shared")
            .unwrap()
            .identity()
            .workspace_id,
        "legacy"
    );
    assert_eq!(
        directory
            .resolve(&b, "legacy")
            .unwrap()
            .attachment()
            .unwrap()
            .as_deref(),
        Some("second-target")
    );
    assert_eq!(
        directory.lifecycle_key(&b, Some("legacy"), None),
        (b.clone(), "shared".into())
    );
    assert_eq!(
        directory.lifecycle_key(&a, Some("legacy"), None),
        (a.clone(), "legacy".into()),
        "retired translation is gone"
    );
    let update = directory.prepare_replace(a.clone(), Vec::new());
    directory.commit_replace(update);
    assert!(
        directory.rows(&a).unwrap().is_empty(),
        "empty differs from a missing list"
    );
    assert!(
        directory.resolve(&b, "legacy").is_ok(),
        "removing A still preserves B"
    );
    assert!(directory.rows("unlisted").is_none());
}

#[test]
fn lifecycle_key_uses_the_event_host_and_keeps_legacy_fallbacks() {
    let mut directory = RowDirectory::default();
    // The same legacy id names different slugs on two hosts.
    for (host, slug) in [("alpha", "one"), ("beta", "two")] {
        let mut row = ws_info(slug, "stored-target");
        row.workspace_id = "legacy".into();
        let mut canonical = ws_info("project", "another-target");
        canonical.workspace_id = format!("canonical-{host}");
        let update = directory.prepare_replace(host.into(), vec![row, canonical]);
        directory.commit_replace(update);
    }
    for (host, slug) in [("alpha", "one"), ("beta", "two")] {
        let key = |hint, default| directory.lifecycle_key(host, hint, default);
        assert_eq!(key(Some("legacy"), None), (host.into(), slug.into()));
        assert_eq!(
            key(Some(&format!("canonical-{host}")), None),
            (host.into(), "project".into())
        );
        assert_eq!(
            key(Some("unknown"), Some("home")),
            (host.into(), "unknown".into())
        );
        assert_eq!(
            key(Some("already-a-slug"), None),
            (host.into(), "already-a-slug".into())
        );
        assert_eq!(
            key(Some(""), None),
            (host.into(), "".into()),
            "an empty lifecycle hint is tolerated"
        );
        assert_eq!(
            key(None, Some("caller-default")),
            (host.into(), "caller-default".into())
        );
        assert_eq!(key(None, None), (host.into(), "<default>".into()));
    }
    assert_eq!(
        directory.lifecycle_key("alpha", Some("canonical-beta"), None),
        ("alpha".into(), "canonical-beta".into())
    );
    assert_eq!(
        directory.lifecycle_key("unlisted", Some("legacy"), None),
        ("unlisted".into(), "legacy".into())
    );
}

#[test]
fn authoritative_replacement_reports_only_retired_canonical_identities() {
    let host = "alpha".to_string();
    let mut directory = RowDirectory::default();
    let replaced = ws_info("same-slug", "old-target");
    let removed = ws_info("removed", "removed-target");
    let retained = ws_info("retained", "retained-target");
    let mut anchor = ws_info("base", "");
    anchor.is_default = true;
    anchor.agent = "none".into();
    let rows = vec![
        replaced.clone(),
        removed.clone(),
        retained.clone(),
        anchor.clone(),
    ];
    for event_host in [host.clone(), "beta".into()] {
        let update = directory.prepare_replace(event_host, rows.clone());
        assert!(update.retired().is_empty(), "first listing retires nothing");
        directory.commit_replace(update);
    }
    let update = directory.prepare_replace(host.clone(), rows);
    assert!(
        update.retired().is_empty(),
        "an unchanged listing retires nothing"
    );
    directory.commit_replace(update);
    let mut successor = replaced.clone();
    successor.workspace_id = "replacement-id".into();
    let update = directory.prepare_replace(host.clone(), vec![successor, retained.clone(), anchor]);
    let retired: std::collections::HashSet<_> = update.retired().iter().cloned().collect();
    let expected: std::collections::HashSet<_> = [replaced.clone(), removed.clone()]
        .into_iter()
        .map(|row| ResultRowIdentity {
            host: host.clone(),
            workspace_id: row.workspace_id,
        })
        .collect();
    assert_eq!(retired, expected);
    assert!(
        directory.resolve(&host, &replaced.workspace_id).is_ok(),
        "preparation leaves the old listing installed for reconciliation"
    );
    directory.commit_replace(update);
    assert!(directory.resolve(&host, &replaced.workspace_id).is_err());
    assert!(directory.resolve(&host, &removed.workspace_id).is_err());
    assert!(directory.resolve(&host, &retained.workspace_id).is_ok());
    assert!(directory
        .resolve(&"beta".into(), &replaced.workspace_id)
        .is_ok());
    assert!(
        !directory.resolve(&host, "default").unwrap().visible(),
        "the kept anchor still resolves"
    );
    assert_eq!(
        directory.lifecycle_key(&host, Some("ws-base-0000"), None),
        (host.clone(), "base".into()),
        "the invisible anchor participates in canonical-id translation"
    );
    assert_eq!(
        directory.lifecycle_key(&host, Some(&removed.workspace_id), None),
        (host, removed.workspace_id)
    );
}
