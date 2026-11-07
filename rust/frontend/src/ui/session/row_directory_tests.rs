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
        assert_eq!(
            directory.lifecycle_key(host, Some("legacy"), None),
            (host.into(), slug.into())
        );
        assert_eq!(
            directory.lifecycle_key(host, Some(&format!("canonical-{host}")), None),
            (host.into(), "project".into())
        );
        assert_eq!(
            directory.lifecycle_key(host, Some("unknown"), Some("home")),
            (host.into(), "unknown".into())
        );
        assert_eq!(
            directory.lifecycle_key(host, Some("already-a-slug"), None),
            (host.into(), "already-a-slug".into())
        );
        assert_eq!(
            directory.lifecycle_key(host, Some(""), None),
            (host.into(), "".into()),
            "an empty lifecycle hint is tolerated"
        );
        assert_eq!(
            directory.lifecycle_key(host, None, Some("caller-default")),
            (host.into(), "caller-default".into())
        );
        assert_eq!(
            directory.lifecycle_key(host, None, None),
            (host.into(), "<default>".into())
        );
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

fn listing(host: &str, slugs: &[&str]) -> RowDirectory {
    let mut directory = RowDirectory::default();
    let rows = slugs.iter().map(|slug| {
        let mut row = ws_info(slug, &format!("attach-{slug}"));
        row.workspace_id = format!("id-{slug}");
        row
    });
    let update = directory.prepare_replace(host.into(), rows.collect());
    directory.commit_replace(update);
    directory
}

fn rows_of(slugs: &[&str]) -> Vec<WorkspaceInfo> {
    slugs.iter().map(|slug| ws_info(slug, &format!("attach-{slug}"))).collect()
}

fn to(host: &str, slug: &str) -> Leave {
    Leave::To((host.into(), slug.into()))
}

/// What the window does with its selected row when a row is destroyed, by its own destroy (`note_destroyed`, no
/// listing) and by the host's next listing: the next row below in the Sessions list, else above, else no session;
/// a destroy of another row, of another host's row, or a first listing changes nothing.
#[test]
fn leaving_a_gone_row_names_its_neighbor_below_else_above() {
    // (listed, selected, destroyed together, expected)
    let table: [(&[&str], &str, &[&str], Leave); 8] = [
        (&["a1", "a2", "a3"], "a1", &["a1"], to("h", "a2")),
        (&["a1", "a2", "a3"], "a2", &["a2"], to("h", "a3")),
        (&["a1", "a2", "a3"], "a3", &["a3"], to("h", "a2")),
        (&["a1", "a2", "a3"], "a1", &["a1", "a2"], to("h", "a3")),
        (&["a1", "a2", "a3"], "a3", &["a3", "a2"], to("h", "a1")),
        (&["a1", "a2", "a3"], "a2", &["a1", "a2", "a3"], Leave::NoSession),
        (&["a1", "a2", "a3"], "a2", &["a3"], Leave::Stay),
        (&["a1"], "a1", &["a1"], Leave::NoSession),
    ];
    for (listed, selected, gone, expected) in table {
        let kept: Vec<&str> = listed.iter().copied().filter(|s| !gone.contains(s)).collect();
        let mut by_own = listing("h", listed);
        for slug in gone {
            by_own.note_destroyed("h", slug);
        }
        assert_eq!(by_own.leave("h", Some(selected), None), expected, "own destroy of {gone:?} on {selected}");
        let by_list = listing("h", listed);
        let update = by_list.prepare_replace("h".into(), rows_of(&kept));
        assert_eq!(by_list.leave("h", Some(selected), Some(&update)), expected, "list without {gone:?} on {selected}");
    }
    let directory = listing("h", &["a1", "a2"]);
    let update = directory.prepare_replace("h".into(), rows_of(&["a2"]));
    assert_eq!(directory.leave("other", Some("a1"), Some(&update)), Leave::Stay, "another host's rows are not its own");
    assert_eq!(directory.leave("h", None, Some(&update)), Leave::Stay, "the default row is never gone");
    assert_eq!(directory.leave("h", Some("never-listed"), Some(&update)), Leave::Stay, "a resumed selection is kept");
    let first = RowDirectory::default();
    let update = first.prepare_replace("h".into(), rows_of(&["a2"]));
    assert_eq!(first.leave("h", Some("a1"), Some(&update)), Leave::Stay, "a first listing drops nothing");
}

/// Two quick destroys before the list answers: the second's neighbor is never the first's row, whether it was named
/// by slug (the reply) or by workspace id (the request); and a same-slug successor in a list keeps the selection.
#[test]
fn a_destroy_in_flight_is_never_the_neighbor() {
    let mut directory = listing("h", &["a1", "a2", "a3"]);
    directory.note_destroyed("h", "id-a2");
    assert_eq!(directory.leave("h", Some("a2"), None), to("h", "a3"));
    directory.note_destroyed("h", "id-a3");
    assert_eq!(directory.leave("h", Some("a3"), None), to("h", "a1"));
    let update = directory.prepare_replace("h".into(), rows_of(&["a1"]));
    directory.commit_replace(update);
    assert_eq!(directory.leave("h", Some("a1"), None), Leave::Stay, "a listing replaces the record");
    let mut successor = ws_info("a1", "attach-new");
    successor.workspace_id = "id-new".into();
    let update = directory.prepare_replace("h".into(), vec![successor]);
    assert_eq!(directory.leave("h", Some("a1"), Some(&update)), Leave::Stay);
}
