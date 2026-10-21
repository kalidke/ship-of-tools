use super::*;

// ---- activity_order (bottom strip within-host ordering) ----

fn ak(host: &str, slug: &str) -> WsKey {
    (host.to_string(), slug.to_string())
}

fn astates(rows: &[(&WsKey, &str, &str)]) -> HashMap<WsKey, (String, String)> {
    rows.iter()
        .map(|(k, st, at)| ((*k).clone(), (st.to_string(), at.to_string())))
        .collect()
}

fn nobadge() -> std::collections::HashSet<WsKey> {
    std::collections::HashSet::new()
}

#[test]
fn activity_order_tiers_red_white_blue_green_purple_gray() {
    let red = ak("h", "red");
    let white = ak("h", "white");
    let blue = ak("h", "blue");
    let green = ak("h", "green");
    let purple = ak("h", "purple");
    let gray = ak("h", "gray");
    // Daemon order is the reverse of the list order; every stamp is newer
    // than the one before, so a stamp-only sort would also be reversed.
    let slugs = vec![
        gray.clone(),
        purple.clone(),
        green.clone(),
        blue.clone(),
        white.clone(),
        red.clone(),
    ];
    let states = astates(&[
        (&gray, "idle", "2026-09-08T09:00:00Z"),
        (&purple, "waiting", "2026-09-08T09:01:00Z"),
        (&green, "working", "2026-09-08T09:02:00Z"),
        (&blue, "done", "2026-09-08T09:03:00Z"),
        (&white, "idle", "2026-09-08T09:04:00Z"),
        (&red, "blocked", "2026-09-08T08:00:00Z"),
    ]);
    let badged = [white.clone()].into_iter().collect();
    let got = activity_order(&slugs, &states, &badged, &[], None);
    assert_eq!(got, vec![red, white, blue, green, purple, gray]);
}

#[test]
fn activity_order_badge_lifts_any_row_but_red_stays_first() {
    let red = ak("h", "red");
    let busy = ak("h", "busy");
    let blue = ak("h", "blue");
    let slugs = vec![blue.clone(), busy.clone(), red.clone()];
    let states = astates(&[
        (&blue, "done", "2026-09-08T09:00:00Z"),
        (&busy, "working", "2026-09-08T09:00:00Z"),
        (&red, "blocked", "2026-09-08T09:00:00Z"),
    ]);
    // A badge on a working row lifts it above done; a badge on the red
    // row changes nothing about its place.
    let badged = [busy.clone(), red.clone()].into_iter().collect();
    assert_eq!(
        activity_order(&slugs, &states, &badged, &[], None),
        vec![red, busy, blue]
    );
}

#[test]
fn activity_order_ranks_by_tier_then_newest_stamp() {
    let a = ak("h", "a");
    let b = ak("h", "b");
    let c = ak("h", "c");
    let d = ak("h", "d");
    let e = ak("h", "e");
    let slugs = vec![a.clone(), b.clone(), c.clone(), d.clone(), e.clone()];
    let states = astates(&[
        (&a, "idle", "2026-09-08T09:00:00Z"),
        (&b, "working", "2026-09-08T08:00:00Z"),
        (&c, "done", "2026-09-08T09:30:00Z"),
        (&d, "blocked", "2026-09-08T08:30:00Z"),
        (&e, "", ""),
    ]);
    let got = activity_order(&slugs, &states, &nobadge(), &[], None);
    // red, blue, green, then the resting rows by stamp with the
    // stampless one last.
    assert_eq!(got, vec![d, c, b, a, e]);
}

#[test]
fn activity_order_never_mixes_host_blocks() {
    let a1 = ak("alpha", "one");
    let a2 = ak("alpha", "two");
    let b1 = ak("beta", "one");
    let b2 = ak("beta", "two");
    let slugs = vec![a1.clone(), a2.clone(), b1.clone(), b2.clone()];
    let states = astates(&[
        (&a1, "idle", "2026-09-08T09:00:00Z"),
        (&a2, "idle", "2026-09-08T10:00:00Z"),
        (&b1, "idle", "2026-09-08T09:00:00Z"),
        (&b2, "working", "2026-09-08T09:00:00Z"),
    ]);
    let got = activity_order(&slugs, &states, &nobadge(), &[], None);
    assert_eq!(got, vec![a2, a1, b2, b1]);
    assert_eq!(
        strip_items(&got, |h| h.clone())
            .iter()
            .filter(|i| matches!(i, StripItem::Bow { .. }))
            .count(),
        2,
        "still exactly two host groups, so two ships"
    );
}

#[test]
fn activity_order_is_stable_across_unchanged_rebuilds_and_daemon_shuffles() {
    let a = ak("h", "a");
    let b = ak("h", "b");
    let c = ak("h", "c");
    let states = astates(&[
        (&a, "idle", "2026-09-08T09:00:00Z"),
        (&b, "idle", "2026-09-08T09:00:00Z"),
        (&c, "idle", "2026-09-08T09:00:00Z"),
    ]);
    let first = activity_order(&[a.clone(), b.clone(), c.clone()], &states, &nobadge(), &[], None);
    assert_eq!(first, vec![a.clone(), b.clone(), c.clone()], "full ties keep daemon order");
    let again = activity_order(&first, &states, &nobadge(), &first, None);
    assert_eq!(again, first, "an unchanged rebuild is byte-identical");
    // The daemon lists the same tied rows in a different order: the
    // previous standing wins, so nothing jitters.
    let shuffled =
        activity_order(&[c.clone(), a.clone(), b.clone()], &states, &nobadge(), &first, None);
    assert_eq!(shuffled, first);
}

#[test]
fn activity_order_pins_the_selected_row_to_its_slot() {
    let a = ak("h", "a");
    let b = ak("h", "b");
    let c = ak("h", "c");
    let prev = vec![a.clone(), b.clone(), c.clone()];
    // c goes working: it should leapfrog to the front, but the selected
    // row b must stay where the cursor has it (index 1).
    let states = astates(&[
        (&a, "idle", "2026-09-08T09:00:00Z"),
        (&b, "idle", "2026-09-08T09:00:00Z"),
        (&c, "working", "2026-09-08T09:05:00Z"),
    ]);
    let got = activity_order(&prev, &states, &nobadge(), &prev, Some(&b));
    assert_eq!(got, vec![c.clone(), b.clone(), a.clone()]);
    // Unpinned, the same change re-ranks b too.
    let free = activity_order(&prev, &states, &nobadge(), &prev, None);
    assert_eq!(free, vec![c, a, b]);
}

#[test]
fn activity_order_pin_clamps_when_the_block_shrinks() {
    let a = ak("h", "a");
    let b = ak("h", "b");
    let c = ak("h", "c");
    let prev = vec![a.clone(), b.clone(), c.clone()];
    let states = astates(&[
        (&a, "working", "2026-09-08T09:00:00Z"),
        (&c, "idle", "2026-09-08T09:00:00Z"),
    ]);
    // b vanished; the selected c sat at index 2, now clamped to 1.
    let got = activity_order(&[a.clone(), c.clone()], &states, &nobadge(), &prev, Some(&c));
    assert_eq!(got, vec![a, c]);
}

#[test]
fn activity_order_new_rows_rank_and_a_first_seen_pin_ranks_too() {
    let a = ak("h", "a");
    let b = ak("h", "b");
    let n = ak("h", "new");
    let prev = vec![a.clone(), b.clone()];
    let states = astates(&[
        (&a, "idle", "2026-09-08T09:00:00Z"),
        (&b, "idle", "2026-09-08T09:00:00Z"),
        (&n, "waiting", "2026-09-08T09:01:00Z"),
    ]);
    let slugs = vec![a.clone(), b.clone(), n.clone()];
    assert_eq!(
        activity_order(&slugs, &states, &nobadge(), &prev, None),
        vec![n.clone(), a.clone(), b.clone()]
    );
    // Selecting the brand-new row (no previous standing) doesn't pin it
    // to a phantom slot — it ranks like any other row.
    assert_eq!(
        activity_order(&slugs, &states, &nobadge(), &prev, Some(&n)),
        vec![n, a, b]
    );
}

#[test]
fn fresh_workspace_caches_two_hosts_sharing_a_slug_do_not_collide() {
    // The invariant this whole slice exists for: two hosts each
    // reporting a workspace named "sot" must produce TWO distinct
    // cache entries, not one clobbering the other.
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    let mut alpha_ws = ws_info("sot", "sot-be-sot");
    alpha_ws.project_root = "/projects/alpha-sot".to_string();
    let mut beta_ws = ws_info("sot", "sot-be-sot");
    beta_ws.project_root = "/projects/beta-sot".to_string();
    lists.insert("alpha".to_string(), vec![alpha_ws]);
    lists.insert("beta".to_string(), vec![beta_ws]);
    let ordered = vec!["alpha".to_string(), "beta".to_string()];
    let fresh = fresh_workspace_caches(&ordered, &lists, &"alpha".to_string());

    assert_eq!(
        fresh.workspace_slugs.len(),
        2,
        "one entry per host, not one merged entry"
    );
    let alpha_key: WsKey = ("alpha".to_string(), "sot".to_string());
    let beta_key: WsKey = ("beta".to_string(), "sot".to_string());
    assert_eq!(
        fresh
            .workspace_project_roots
            .get(&alpha_key)
            .map(String::as_str),
        Some("/projects/alpha-sot")
    );
    assert_eq!(
        fresh
            .workspace_project_roots
            .get(&beta_key)
            .map(String::as_str),
        Some("/projects/beta-sot")
    );
}

#[test]
fn a_rows_root_is_the_daemons_spelling_so_a_watched_path_resolves_under_it() {
    // A row created through a link above its folder (macOS's /tmp and
    // /var), or in another case or 8.3 name on Windows: the daemon's
    // watcher spells its paths as `canonical_root`, and an umbrella
    // row's copy of a change is matched against this root as strings.
    let host = "h".to_string();
    let mut linked = ws_info("alpha", "sot-be-alpha");
    linked.project_root = "/t/link/repo".to_string();
    linked.canonical_root = Some("/t/real/repo".to_string());
    let older = ws_info("beta", "sot-be-beta");
    let lists = HashMap::from([(host.clone(), vec![linked, older])]);
    let fresh = fresh_workspace_caches(std::slice::from_ref(&host), &lists, &host);
    let root = |slug: &str| fresh.workspace_project_roots.get(&(host.clone(), slug.to_string())).cloned();
    assert_eq!(root("alpha").as_deref(), Some("/t/real/repo"));
    assert_eq!(root("beta").as_deref(), Some("/projects/beta"), "a daemon that sends no canonical_root");
    let event = Some("/t/real/repo/src/a.jl");
    assert_eq!(
        resolve_preview_changed(Some("umbrella"), Some("files:repo/src/a.jl"), event, Some("alpha"), root("alpha").as_deref()),
        Some("files:src/a.jl".to_string())
    );
}

#[test]
fn fresh_workspace_caches_default_slug_is_scoped_to_active_host() {
    // Each host's OWN `is_default` row exists; only active_host's
    // should set `default_workspace_slug` (a bare slug — the pair
    // with `active_host` is what the caller actually needs).
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    let mut alpha_default = ws_info("home", "sot-be-home");
    alpha_default.is_default = true;
    let mut beta_default = ws_info("root", "sot-be-root");
    beta_default.is_default = true;
    lists.insert("alpha".to_string(), vec![alpha_default]);
    lists.insert("beta".to_string(), vec![beta_default]);
    let ordered = vec!["alpha".to_string(), "beta".to_string()];

    let fresh_alpha = fresh_workspace_caches(&ordered, &lists, &"alpha".to_string());
    assert_eq!(fresh_alpha.default_workspace_slug.as_deref(), Some("home"));

    let fresh_beta = fresh_workspace_caches(&ordered, &lists, &"beta".to_string());
    assert_eq!(fresh_beta.default_workspace_slug.as_deref(), Some("root"));
}

#[test]
fn fresh_workspace_caches_workspace_id_slugs_resolves_to_the_right_host() {
    // The canonical workspace_id → WsKey translation (repl.frame
    // lifecycle routing) must carry the ORIGINATING host, not
    // whichever host happens to be active. ADR 0042 L2a codex review,
    // item J: the map's OWN key is host-qualified too, so a same-id
    // lookup can only ever resolve against the querying host's own
    // entry (a legacy id is bare-slug and DOES collide across hosts).
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert("beta".to_string(), vec![ws_info("sot", "sot-be-sot")]);
    let ordered = vec!["beta".to_string()];
    let fresh = fresh_workspace_caches(&ordered, &lists, &"alpha".to_string());
    assert_eq!(
        fresh
            .workspace_id_slugs
            .get(&("beta".to_string(), "ws-sot-0000".to_string())),
        Some(&("beta".to_string(), "sot".to_string()))
    );
    // A query for the SAME canonical id under a DIFFERENT host misses
    // entirely -- the collision a bare-id key used to paper over.
    assert_eq!(
        fresh
            .workspace_id_slugs
            .get(&("alpha".to_string(), "ws-sot-0000".to_string())),
        None,
        "the same canonical id under a different host must not resolve"
    );
}

#[test]
fn union_replace_leaves_the_other_hosts_list_intact() {
    // The actual per-host-replace step (`workspace_lists.insert`) is a
    // bare HashMap insert — this test proves the INVARIANT it exists
    // for: replacing host A's slice must not disturb host B's, exactly
    // like a real `IncomingEvt::Workspaces` reply from A shouldn't
    // clobber B's last-known (possibly now-unreachable) list.
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert("alpha".to_string(), vec![ws_info("one", "sot-be-one")]);
    lists.insert("beta".to_string(), vec![ws_info("two", "sot-be-two")]);

    // A fresh reply from alpha with a DIFFERENT workspace set.
    lists.insert("alpha".to_string(), vec![ws_info("three", "sot-be-three")]);

    assert_eq!(lists.get("beta").map(Vec::len), Some(1));
    assert_eq!(lists["beta"][0].slug, "two", "beta's list is untouched");
    assert_eq!(
        lists["alpha"][0].slug, "three",
        "alpha's list is the new one"
    );
}

#[test]
fn fresh_workspace_caches_skips_a_host_with_no_list_yet() {
    // ordered_hosts includes every connection, but a host that hasn't
    // answered workspace.list yet (still mid-hello) contributes no
    // rows rather than panicking on a missing map entry.
    let lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    let ordered = vec!["alpha".to_string(), "beta".to_string()];
    let fresh = fresh_workspace_caches(&ordered, &lists, &"alpha".to_string());
    assert!(fresh.workspace_slugs.is_empty());
    assert!(fresh.default_workspace_slug.is_none());
}

#[test]
fn fresh_workspace_caches_hides_the_inert_anchor_from_the_strip_but_keeps_its_default_slug() {
    // The bottom session strip is built from workspace_slugs/labels,
    // NOT from session_host_children's tree — so the same inert-anchor
    // amendment (2026-09-04) must be applied here too, or the strip
    // still shows a row the tree hides (the bug this test module is
    // extended for). `default_workspace_slug` must still resolve to
    // the anchor: it's the strip's own active-index fallback.
    let host: HostKey = "local".to_string();
    let mut anchor = ws_info("sot", "sot-be-sot");
    anchor.is_default = true;
    anchor.agent = "none".to_string();
    anchor.runtime = "capsule".to_string();
    let list = vec![anchor];
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert(host.clone(), list);
    let ordered = vec![host.clone()];
    let fresh = fresh_workspace_caches(&ordered, &lists, &host);

    let anchor_key: WsKey = (host.clone(), "sot".to_string());
    assert!(
        fresh.workspace_slugs.is_empty(),
        "the inert anchor must not be pushed into workspace_slugs: {:?}",
        fresh.workspace_slugs
    );
    assert!(
        !fresh.workspace_labels.contains_key(&anchor_key),
        "the inert anchor must get no label"
    );
    assert_eq!(
        fresh.default_workspace_slug.as_deref(),
        Some("sot"),
        "default_workspace_slug must still name the anchor — the strip's own \
             active-index fallback needs it"
    );
}

#[test]
fn fresh_workspace_caches_hides_a_default_tmux_row_with_no_agent_from_the_strip_too() {
    // Decided (2026-09-06, symmetry): same anchor rule as the tree,
    // on every runtime -- the strip never lists the default row without
    // an agent, but `default_workspace_slug` still names it (the strip's
    // own active-index fallback).
    let host: HostKey = "local".to_string();
    let mut default_row = ws_info("sot", "sot-be-sot");
    default_row.is_default = true;
    default_row.agent = "none".to_string();
    default_row.runtime = "tmux".to_string();
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert(host.clone(), vec![default_row]);
    let ordered = vec![host.clone()];
    let fresh = fresh_workspace_caches(&ordered, &lists, &host);

    let key: WsKey = (host.clone(), "sot".to_string());
    assert!(!fresh.workspace_slugs.contains(&key), "{:?}", fresh.workspace_slugs);
    assert!(!fresh.workspace_labels.contains_key(&key));
    assert_eq!(fresh.default_workspace_slug.as_deref(), Some("sot"));
}
