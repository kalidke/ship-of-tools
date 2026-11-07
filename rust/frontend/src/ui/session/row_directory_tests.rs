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

/// A host's listing as the daemon sends it: the inert default first, then the sessions by slug.
fn rows_of(slugs: &[&str]) -> Vec<WorkspaceInfo> {
    let mut default = ws_info("default", "attach-default");
    default.is_default = true;
    default.agent = "none".into();
    let sessions = slugs.iter().map(|slug| {
        let mut row = ws_info(slug, &format!("attach-{slug}"));
        row.workspace_id = format!("id-{slug}");
        row
    });
    std::iter::once(default).chain(sessions).collect()
}

fn listing(lists: &[(&str, &[&str])]) -> RowDirectory {
    let mut directory = RowDirectory::default();
    for (host, slugs) in lists {
        let update = directory.prepare_replace((*host).into(), rows_of(slugs));
        directory.commit_replace(update);
    }
    directory
}

fn hosts() -> Vec<HostKey> {
    vec!["h".to_string(), "g".to_string()]
}

fn to(host: &str, slug: &str) -> Leave {
    Leave::To((host.into(), slug.into()))
}

const LISTED: [(&str, &[&str]); 2] = [("h", &["a1", "a2", "a3"]), ("g", &["b1"])];

/// What the window does with its selected row when a row is destroyed, by its own destroy's reply and by the host's
/// next listing: the next session in the whole Sessions list (computers in order, each default first then by slug),
/// below else above; no session only when none remains anywhere. A destroy of another row, of another computer's
/// row, a first listing or a never-listed selection changes nothing.
#[test]
fn leaving_a_gone_row_names_its_neighbor_in_the_whole_sessions_list() {
    // (selected host, selected, destroyed together on h, expected)
    let table: [(&str, &str, &[&str], Leave); 8] = [
        ("h", "a1", &["a1"], to("h", "a2")),
        ("h", "a2", &["a2"], to("h", "a3")),
        // Last of its computer: the next computer's first session, never the inert default between.
        ("h", "a3", &["a3"], to("g", "b1")),
        ("h", "a1", &["a1", "a2"], to("h", "a3")),
        ("h", "a2", &["a2", "a3"], to("g", "b1")),
        ("h", "a2", &["a3"], Leave::Stay),
        ("h", "a2", &["a1"], Leave::Stay),
        ("g", "b1", &["a1"], Leave::Stay),
    ];
    for (host, selected, gone, expected) in table {
        let kept: Vec<&str> = LISTED[0].1.iter().copied().filter(|s| !gone.contains(s)).collect();
        let mut by_own = listing(&LISTED);
        for slug in gone {
            by_own.note_destroyed("h", slug);
        }
        let replied = by_own.leave(&hosts(), (host, Some(selected)), "h", Gone::Replied(gone[0]));
        if gone.contains(&selected) {
            assert_eq!(replied, expected, "own destroy of {gone:?} on {host}/{selected}");
        } else {
            assert_eq!(replied, Leave::Stay, "own destroy of {gone:?} on {host}/{selected}");
        }
        let by_list = listing(&LISTED);
        let update = by_list.prepare_replace("h".into(), rows_of(&kept));
        let listed = by_list.leave(&hosts(), (host, Some(selected)), "h", Gone::Listed(&update));
        assert_eq!(listed, expected, "list without {gone:?} on {host}/{selected}");
    }
    // The last session of the last computer goes up to the one above; the only session anywhere has no neighbor.
    let mut last = listing(&LISTED);
    last.note_destroyed("g", "b1");
    assert_eq!(last.leave(&hosts(), ("g", Some("b1")), "g", Gone::Replied("b1")), to("h", "a3"));
    let mut only = listing(&[("h", &["a1"])]);
    only.note_destroyed("h", "a1");
    assert_eq!(only.leave(&["h".to_string()], ("h", Some("a1")), "h", Gone::Replied("a1")), Leave::NoSession);
    // The default row and a selection resumed before the first list are never gone; a first list drops nothing.
    let directory = listing(&LISTED);
    let update = directory.prepare_replace("h".into(), rows_of(&["a2", "a3"]));
    assert_eq!(directory.dropped("h", &update), ["a1"], "the rows a listing lost are forgotten, a first listing's none");
    assert!(RowDirectory::default().dropped("h", &update).is_empty());
    assert_eq!(directory.leave(&hosts(), ("h", None), "h", Gone::Listed(&update)), Leave::Stay);
    assert_eq!(directory.leave(&hosts(), ("h", Some("never-listed")), "h", Gone::Listed(&update)), Leave::Stay);
    let first = RowDirectory::default();
    let update = first.prepare_replace("h".into(), rows_of(&["a2"]));
    assert_eq!(first.leave(&hosts(), ("h", Some("a1")), "h", Gone::Listed(&update)), Leave::Stay);
}

/// A destroy note lives until a listing lacks its row or the daemon refuses the destroy: through an unrelated list
/// that has both rows, the first reply never moves onto the row being destroyed; after a refused destroy a later
/// destroy of another row never moves the view off a living session.
#[test]
fn a_destroy_in_flight_is_never_the_neighbor() {
    let mut directory = listing(&LISTED);
    // The requests name the rows by workspace id.
    directory.note_destroyed("h", "id-a2");
    directory.note_destroyed("h", "id-a3");
    let unrelated = directory.prepare_replace("h".into(), rows_of(&["a1", "a2", "a3"]));
    directory.commit_replace(unrelated);
    assert_eq!(directory.leave(&hosts(), ("h", Some("a2")), "h", Gone::Replied("a2")), to("g", "b1"));
    let update = directory.prepare_replace("h".into(), rows_of(&["a1", "a3"]));
    assert_eq!(directory.leave(&hosts(), ("h", Some("a2")), "h", Gone::Listed(&update)), to("g", "b1"));
    directory.commit_replace(update);
    // a2 left the listing, so its note is gone; a3 is still being destroyed.
    assert_eq!(directory.leave(&hosts(), ("h", Some("a1")), "h", Gone::Replied("a2")), Leave::Stay);
    directory.note_destroyed("h", "a1");
    assert_eq!(directory.leave(&hosts(), ("h", Some("a1")), "h", Gone::Replied("a1")), to("g", "b1"));
    // A refused destroy of the shown a2, then a destroy of a3 answered: the view stays on a2.
    let mut refused = listing(&LISTED);
    refused.note_destroyed("h", "id-a2");
    refused.clear_destroyed("h");
    refused.note_destroyed("h", "a3");
    assert_eq!(refused.leave(&hosts(), ("h", Some("a2")), "h", Gone::Replied("a3")), Leave::Stay);
    // The refused row is alive, so it is the neighbor of the shown row destroyed next.
    refused.note_destroyed("h", "a1");
    assert_eq!(refused.leave(&hosts(), ("h", Some("a1")), "h", Gone::Replied("a1")), to("h", "a2"));
    // A row of the same name on another computer is not the selected one.
    let mut twins = listing(&[("h", &["a1", "a2"]), ("g", &["a1"])]);
    twins.note_destroyed("h", "a1");
    assert_eq!(twins.leave(&hosts(), ("g", Some("a1")), "h", Gone::Replied("a1")), Leave::Stay);
    let update = twins.prepare_replace("h".into(), rows_of(&["a2"]));
    assert_eq!(twins.leave(&hosts(), ("g", Some("a1")), "h", Gone::Listed(&update)), Leave::Stay);
    // A same-slug successor in a listing keeps the selection.
    let mut successor = ws_info("a2", "attach-new");
    successor.workspace_id = "id-new".into();
    let update = refused.prepare_replace("h".into(), vec![successor]);
    assert_eq!(refused.leave(&hosts(), ("h", Some("a2")), "h", Gone::Listed(&update)), Leave::Stay);
}
