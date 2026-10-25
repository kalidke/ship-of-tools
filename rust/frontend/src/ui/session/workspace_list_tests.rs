use super::super::row_presentation::PresentationEffects;
use super::*;
use crate::net::transport::WorkspaceInfo;

fn key(host: &str, slug: &str) -> WsKey {
    (host.to_string(), slug.to_string())
}

fn row(slug: &str, state: &str, at: &str) -> WorkspaceInfo {
    let mut row = ws_info(slug, &format!("attach-{slug}"));
    row.agent_state = state.to_string();
    row.agent_status_at = at.to_string();
    row
}

fn replace(directory: &mut RowDirectory, host: &str, rows: Vec<WorkspaceInfo>) {
    let update = directory.prepare_replace(host.to_string(), rows);
    directory.commit_replace(update);
}

fn project(
    p: &mut RowPresentation,
    d: &RowDirectory,
    hosts: &[&str],
    active: &str,
    slug: Option<&str>,
) -> PresentationEffects {
    let hosts: Vec<_> = hosts.iter().map(|h| h.to_string()).collect();
    p.refresh(
        d,
        &hosts,
        active,
        slug,
        Default::default(),
        std::time::Instant::now(),
    )
}

#[test]
fn canonical_translation_includes_a_host_outside_ordered_hosts() {
    let mut directory = RowDirectory::default();
    replace(
        &mut directory,
        "connected",
        vec![row("connected-row", "", "")],
    );
    replace(
        &mut directory,
        "synthetic",
        vec![row("synthetic-row", "", "")],
    );
    let mut p = RowPresentation::default();
    project(&mut p, &directory, &["connected"], "connected", None);
    assert_eq!(p.keys(), &[key("connected", "connected-row")]);
    assert_eq!(p.project_root("synthetic", "synthetic-row"), None);
    assert_eq!(
        directory.lifecycle_key("synthetic", Some("ws-synthetic-row-0000"), None),
        key("synthetic", "synthetic-row")
    );
}

#[test]
fn projection_keeps_hosts_distinct_and_obeys_display_order() {
    let mut d = RowDirectory::default();
    let mut a = row("shared", "working", "alpha-stamp");
    a.label = "Alpha".into();
    a.project_root = "/projects/alpha".into();
    let mut b = row("shared", "blocked", "beta-stamp");
    b.label = "Beta".into();
    b.project_root = "/projects/beta".into();
    replace(&mut d, "alpha", vec![a]);
    replace(&mut d, "beta", vec![b]);
    replace(&mut d, "synthetic", vec![row("hidden", "done", "")]);
    let mut p = RowPresentation::default();
    project(&mut p, &d, &["beta", "unanswered", "alpha"], "alpha", None);
    assert_eq!(p.keys(), &[key("beta", "shared"), key("alpha", "shared")]);
    for (host, label, root, state, stamp) in [
        (
            "alpha",
            "Alpha",
            "/projects/alpha",
            "working",
            "alpha-stamp",
        ),
        ("beta", "Beta", "/projects/beta", "blocked", "beta-stamp"),
    ] {
        assert_eq!(p.label(host, "shared"), Some(label));
        assert_eq!(p.project_root(host, "shared"), Some(root));
        assert_eq!(p.state(host, "shared"), Some((state, stamp)));
    }
    assert_eq!(p.state("synthetic", "hidden"), None);
    assert_eq!(p.label("synthetic", "hidden"), None);
    assert_eq!(p.project_root("synthetic", "hidden"), None);
    replace(
        &mut d,
        "alpha",
        vec![row("replacement", "idle", "new-stamp")],
    );
    project(&mut p, &d, &["beta", "unanswered", "alpha"], "alpha", None);
    assert_eq!(
        p.keys(),
        &[key("beta", "shared"), key("alpha", "replacement")]
    );
    assert_eq!(p.label("alpha", "shared"), None);
    assert_eq!(p.project_root("alpha", "shared"), None);
    assert_eq!(p.state("alpha", "shared"), None);
    assert_eq!(p.label("beta", "shared"), Some("Beta"));
    assert_eq!(p.project_root("beta", "shared"), Some("/projects/beta"));
    assert_eq!(p.state("beta", "shared"), Some(("blocked", "beta-stamp")));
}

#[test]
fn inert_defaults_are_hidden_but_keep_their_metadata_and_default() {
    for runtime in ["capsule", "tmux"] {
        let mut d = RowDirectory::default();
        let mut anchor = row("anchor", "idle", "stamp");
        anchor.is_default = true;
        anchor.agent = "none".into();
        anchor.runtime = runtime.into();
        replace(&mut d, "local", vec![anchor]);
        let mut p = RowPresentation::default();
        project(&mut p, &d, &["local", "unanswered"], "local", None);
        assert!(p.keys().is_empty(), "{runtime}");
        assert_eq!(p.label("local", "anchor"), None);
        assert_eq!(p.project_root("local", "anchor"), Some("/projects/anchor"));
        assert_eq!(p.state("local", "anchor"), Some(("idle", "stamp")));
        assert_eq!(p.default_slug(), Some("anchor"));
    }
}

#[test]
fn labels_keep_slug_and_hello_fallbacks() {
    let mut d = RowDirectory::default();
    let mut named = row("named", "", "");
    named.label = "Friendly".into();
    named.is_default = true;
    replace(&mut d, "h", vec![named, row("bare", "", "")]);
    let mut p = RowPresentation::default();
    project(&mut p, &d, &["h"], "h", None);
    assert_eq!(
        p.active_label("h", Some("named"), Some("Hello")),
        Some("Friendly".into())
    );
    assert_eq!(p.active_label("h", Some("bare"), None), Some("bare".into()));
    assert_eq!(
        p.active_label("h", Some("unknown"), Some("Hello")),
        Some("unknown".into())
    );
    assert_eq!(
        p.active_label("other", Some("named"), Some("Hello")),
        Some("named".into())
    );
    assert_eq!(
        p.active_label("h", None, Some("Hello")),
        Some("Hello".into())
    );
    assert_eq!(p.active_label("h", None, None), None);
}

#[test]
fn default_slug_tracks_the_selected_dial_host() {
    let mut d = RowDirectory::default();
    for (host, slug) in [("a", "home"), ("b", "root")] {
        let mut r = row(slug, "", "");
        r.is_default = true;
        replace(&mut d, host, vec![r]);
    }
    let mut p = RowPresentation::default();
    for _ in 0..2 {
        let effects = project(&mut p, &d, &["a", "b"], "a", None);
        assert_eq!(p.default_slug(), Some("home"));
        assert_eq!(
            effects.default_migration,
            Some(key("a", "home")),
            "every refresh exports migration, even without a default change"
        );
    }
    let keys = p.keys().to_vec();
    p.select_host_default(&d, "b");
    assert_eq!(p.default_slug(), Some("root"));
    assert_eq!(ws_key_of(Some("root"), p.default_slug()), "<default>");
    assert_eq!(ws_key_of(Some("home"), p.default_slug()), "home");
    assert_eq!(p.selected_key("b", None), Some(key("b", "root")));
    assert_eq!(
        p.keys(),
        keys.as_slice(),
        "switching does not rebuild projections"
    );
    p.select_host_default(&d, "unanswered");
    assert_eq!(p.default_slug(), None);
    assert_eq!(p.keys(), keys.as_slice());
    replace(&mut d, "b", vec![]);
    let effects = project(&mut p, &d, &["a", "b"], "b", None);
    assert_eq!(
        p.default_slug(),
        None,
        "another host's default must not win"
    );
    assert_eq!(effects.default_migration, None);
    assert_eq!(ws_key_of(Some("root"), p.default_slug()), "root");
}

#[test]
fn canonical_roots_and_missing_root_fallbacks_are_preserved() {
    let mut d = RowDirectory::default();
    let mut linked = row("alpha", "", "");
    linked.project_root = "/t/link/repo".into();
    linked.canonical_root = Some("/t/real/repo".into());
    linked.is_default = true;
    replace(&mut d, "h", vec![linked, row("older", "", "")]);
    let mut p = RowPresentation::default();
    project(&mut p, &d, &["h"], "h", None);
    let root = p.active_project_root("h", Some("alpha"), Some("/hello"));
    assert_eq!(root, Some("/t/real/repo"));
    assert_eq!(
        resolve_preview_changed(
            Some("umbrella"),
            Some("files:repo/src/a.jl"),
            Some("/t/real/repo/src/a.jl"),
            Some("alpha"),
            root
        ),
        Some("files:src/a.jl".into())
    );
    assert_eq!(
        p.active_project_root("h", Some("older"), Some("/hello")),
        Some("/projects/older")
    );
    assert_eq!(
        p.active_project_root("h", Some("unknown"), Some("/hello")),
        None
    );
    assert_eq!(
        p.active_project_root("h", None, Some("/hello")),
        Some("/hello")
    );
    assert_eq!(p.active_project_root("h", None, None), None);
    p.forget_destroyed_metadata(&key("h", "alpha"));
    assert_eq!(
        p.active_project_root("h", Some("alpha"), Some("/hello")),
        Some("/hello")
    );
    assert_eq!(p.active_project_root("h", Some("alpha"), None), None);
}

#[test]
fn activity_rank_keeps_red_then_badge_blue_green_purple_rest() {
    let mut d = RowDirectory::default();
    let rows = [
        ("gray", "idle"),
        ("purple", "waiting"),
        ("green", "working"),
        ("blue", "done"),
        ("badge", "idle"),
        ("red", "blocked"),
        ("unknown", "unknown"),
        ("empty", ""),
    ];
    replace(
        &mut d,
        "h",
        rows.iter()
            .map(|(s, state)| row(s, state, "2026-09-08T09:00:00Z"))
            .collect(),
    );
    let mut p = RowPresentation::default();
    p.refresh(
        &d,
        &["h".into()],
        "absent",
        None,
        [key("h", "badge")].into_iter().collect(),
        std::time::Instant::now(),
    );
    let expected = |slugs: &[&str]| slugs.iter().map(|s| key("h", s)).collect::<Vec<_>>();
    assert_eq!(
        p.keys(),
        expected(&["red", "badge", "blue", "green", "purple", "gray", "unknown", "empty"])
    );
    p.resort(
        "absent",
        None,
        [key("h", "green"), key("h", "red")].into_iter().collect(),
    );
    assert_eq!(
        p.keys(),
        expected(&["red", "green", "blue", "purple", "badge", "gray", "unknown", "empty"])
    );
    p.resort("absent", None, Default::default());
    assert_eq!(
        p.keys(),
        expected(&["red", "blue", "green", "purple", "badge", "gray", "unknown", "empty"])
    );
}

#[test]
fn activity_order_preserves_host_blocks_and_stable_ties() {
    let mut d = RowDirectory::default();
    replace(
        &mut d,
        "a",
        vec![
            row("one", "idle", "2026-09-08T10:30:00+02:00"),
            row("two", "idle", "2026-09-08T09:00:00Z"),
            row("tie", "idle", "2026-09-08T09:00:00Z"),
            row("bad", "idle", "invalid"),
            row("missing", "idle", ""),
        ],
    );
    replace(
        &mut d,
        "b",
        vec![
            row("one", "idle", "2026-09-08T09:00:00Z"),
            row("two", "working", "2026-09-08T09:00:00Z"),
        ],
    );
    let mut p = RowPresentation::default();
    project(&mut p, &d, &["b", "a"], "absent", None);
    let expected = vec![
        key("b", "two"),
        key("b", "one"),
        key("a", "two"),
        key("a", "tie"),
        key("a", "one"),
        key("a", "bad"),
        key("a", "missing"),
    ];
    assert_eq!(p.keys(), expected);
    assert_eq!(
        strip_items(p.keys(), |h| h.clone())
            .iter()
            .filter(|i| matches!(i, StripItem::Bow { .. }))
            .count(),
        2
    );
    let shuffled = d.rows("a").unwrap().iter().rev().cloned().collect();
    replace(&mut d, "a", shuffled);
    for _ in 0..2 {
        project(&mut p, &d, &["b", "a"], "absent", None);
        assert_eq!(p.keys(), expected);
        p.resort("absent", None, Default::default());
        assert_eq!(p.keys(), expected);
    }
}

#[test]
fn selected_row_keeps_its_previous_host_local_slot() {
    let mut d = RowDirectory::default();
    replace(&mut d, "other", vec![row("b", "blocked", "")]);
    replace(
        &mut d,
        "h",
        vec![
            row("a", "idle", ""),
            row("b", "idle", ""),
            row("c", "idle", ""),
        ],
    );
    let mut p = RowPresentation::default();
    project(&mut p, &d, &["other", "h"], "h", Some("b"));
    replace(
        &mut d,
        "h",
        vec![
            row("a", "idle", ""),
            row("b", "idle", ""),
            row("c", "working", ""),
        ],
    );
    project(&mut p, &d, &["other", "h"], "h", Some("b"));
    assert_eq!(
        p.keys(),
        &[
            key("other", "b"),
            key("h", "c"),
            key("h", "b"),
            key("h", "a")
        ]
    );
    p.resort("h", Some("b"), [key("h", "a")].into_iter().collect());
    assert_eq!(
        p.keys(),
        &[
            key("other", "b"),
            key("h", "a"),
            key("h", "b"),
            key("h", "c")
        ]
    );
    replace(
        &mut d,
        "h",
        vec![row("b", "blocked", ""), row("c", "idle", "")],
    );
    project(&mut p, &d, &["other", "h"], "h", Some("c"));
    assert_eq!(
        p.keys(),
        &[key("other", "b"), key("h", "b"), key("h", "c")],
        "previous slot 2 clamps to 1"
    );
    let mut default = row("new", "waiting", "");
    default.is_default = true;
    replace(
        &mut d,
        "h",
        vec![row("b", "idle", ""), row("c", "idle", ""), default],
    );
    project(&mut p, &d, &["other", "h"], "h", None);
    assert_eq!(
        p.keys(),
        &[
            key("other", "b"),
            key("h", "new"),
            key("h", "b"),
            key("h", "c")
        ],
        "first-seen selection ranks normally"
    );
    let mut default = row("b", "blocked", "");
    default.is_default = true;
    replace(
        &mut d,
        "h",
        vec![row("c", "working", ""), default, row("new", "idle", "")],
    );
    project(&mut p, &d, &["other", "h"], "h", None);
    assert_eq!(
        p.keys(),
        &[
            key("other", "b"),
            key("h", "c"),
            key("h", "b"),
            key("h", "new")
        ],
        "pin uses the fresh default"
    );
}

#[test]
fn next_row_wraps_and_uses_host_and_default_selection() {
    let mut d = RowDirectory::default();
    let mut default = row("same", "", "");
    default.is_default = true;
    replace(&mut d, "a", vec![default.clone()]);
    replace(&mut d, "b", vec![default]);
    let mut p = RowPresentation::default();
    assert_eq!(p.next_row(1, "a", None), None);
    project(&mut p, &d, &["a"], "a", None);
    assert_eq!(p.next_row(1, "a", None), None);
    project(&mut p, &d, &["a", "b"], "b", None);
    assert_eq!(p.next_row(1, "b", None), Some(key("a", "same")));
    assert_eq!(p.next_row(-1, "a", Some("same")), Some(key("b", "same")));
    assert_eq!(p.next_row(1, "unknown", None), Some(key("b", "same")));
    assert_eq!(p.next_row(2, "b", None), Some(key("b", "same")));
}
