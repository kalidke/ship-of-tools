use super::*;

// State-nav (ADR 0023): agent work-state tone + staleness aging.
fn agent_payload(state: &str, status_at: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut p = serde_json::Map::new();
    p.insert(
        "agent_state".into(),
        serde_json::Value::String(state.into()),
    );
    p.insert(
        "agent_status_at".into(),
        serde_json::Value::String(status_at.into()),
    );
    p
}

#[test]
fn agent_tone_maps_states_and_absence() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T18:40:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let fresh = "2026-06-15T18:39:00Z"; // 1 min ago
    assert_eq!(
        agent_tone_for(&agent_payload("working", fresh), now),
        Some((AgentTone::Working, false))
    );
    assert_eq!(
        agent_tone_for(&agent_payload("idle", fresh), now),
        Some((AgentTone::Idle, false))
    );
    assert_eq!(
        agent_tone_for(&agent_payload("blocked", fresh), now),
        Some((AgentTone::Blocked, false))
    );
    assert_eq!(
        agent_tone_for(&agent_payload("done", fresh), now),
        Some((AgentTone::Done, false))
    );
    // No state, unknown state, and a wholly empty payload → render as a
    // normal row (no tone).
    assert_eq!(agent_tone_for(&agent_payload("", fresh), now), None);
    assert_eq!(agent_tone_for(&agent_payload("spinning", fresh), now), None);
    assert_eq!(agent_tone_for(&serde_json::Map::new(), now), None);
}

#[test]
fn agent_working_wilts_when_stale_others_do_not() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T19:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let old = "2026-06-15T18:40:00Z"; // 20 min ago > AGENT_STALE_MINUTES
                                      // A working agent that hasn't re-stamped in >10 min wilts.
    assert_eq!(
        agent_tone_for(&agent_payload("working", old), now),
        Some((AgentTone::Working, true))
    );
    // Idle/done/blocked never wilt — only "working" ages.
    assert_eq!(
        agent_tone_for(&agent_payload("idle", old), now),
        Some((AgentTone::Idle, false))
    );
    assert_eq!(
        agent_tone_for(&agent_payload("blocked", old), now),
        Some((AgentTone::Blocked, false))
    );
    // A working agent with a missing/garbage timestamp is treated as
    // fresh (don't wilt on a parse failure).
    assert_eq!(
        agent_tone_for(&agent_payload("working", ""), now),
        Some((AgentTone::Working, false))
    );
    assert_eq!(
        agent_tone_for(&agent_payload("working", "not-a-date"), now),
        Some((AgentTone::Working, false))
    );
}

#[test]
fn foreign_capsule_row_takes_the_skew_style() {
    // ADR 0030 §8 decision 31c: `[foreign]` renders — the tag comes
    // from `capsule_phase_tag` — and the row itself takes the SAME
    // yellow the strip line uses for FE/BE skew (`is_stale`'s branch,
    // checked before agent tone in the draw loop).
    assert_eq!(capsule_phase_tag("foreign"), "[foreign]");
    assert!(capsule_row_is_foreign("session", Some("foreign")));
    // Never foreign off a session row, even with the same phase text —
    // only `build_session_row` ever populates a `phase` payload key.
    assert!(!capsule_row_is_foreign("file", Some("foreign")));
    // Every other phase value on a session row is not foreign either.
    for phase in ["ready", "unreachable", "stopped", "starting"] {
        assert!(!capsule_row_is_foreign("session", Some(phase)));
    }
    assert!(!capsule_row_is_foreign("session", None));
}

/// Per-session accounts (owner-simplified brief, 2026-09-15):
/// `ws_info` plus a non-default account, for the sessions-list suffix
/// tests.
fn ws_info_with_account(
    slug: &str,
    session_name: &str,
    account: &str,
) -> crate::net::transport::WorkspaceInfo {
    crate::net::transport::WorkspaceInfo {
        account: account.to_string(),
        ..ws_info(slug, session_name)
    }
}

/// Per-session accounts (owner-simplified brief, 2026-09-15): the
/// sessions-list suffix. A default (empty-account) row renders exactly
/// as before; a non-default row gets a short `· <name>` suffix so the
/// user can see which subscription it spends.
#[test]
fn build_session_row_omits_account_suffix_for_the_default_directory() {
    let w = ws_info("proj", "sot-be-proj"); // account == "" (default)
    let node = State::build_session_row(&"local".to_string(), &w);
    assert_eq!(node.label, "proj", "unchanged from before the accounts field existed");
}

#[test]
fn build_session_row_appends_the_account_suffix_when_not_default() {
    let w = ws_info_with_account("proj", "sot-be-proj", "team");
    let node = State::build_session_row(&"local".to_string(), &w);
    assert!(
        node.label.ends_with("· team"),
        "expected a trailing account suffix, got: {}",
        node.label
    );
}

#[test]
fn session_host_children_hides_the_default_capsule_row_when_it_has_no_agent() {
    // 2026-09-04 amendment: the default CAPSULE row with `agent ==
    // "none"` is the daemon's inert anchor, not a session — it must
    // not appear in the Sessions tree at all, not even under its own
    // kind. Only `[+ create new]` plus the user's own sessions
    // should come back.
    let host: HostKey = "local".to_string();
    let mut anchor = ws_info("sot", "sot-be-sot");
    anchor.is_default = true;
    anchor.agent = "none".to_string();
    anchor.runtime = "capsule".to_string();
    let mut user_session = ws_info("alpha", "sot-be-alpha");
    user_session.agent = "claude".to_string();
    let list = vec![anchor, user_session];
    let kids = session_host_children(&host, &list);
    let kinds: Vec<&str> = kids.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, vec!["session_create", "session"]);
    assert!(kids.iter().all(|n| !n.id.contains("sot-be-sot")));
    assert!(kids.iter().any(|n| n.id.contains("sot-be-alpha")));
}

#[test]
fn session_host_children_keeps_the_default_row_when_it_still_has_an_agent() {
    // An existing capsule box that hasn't converged past the old
    // seed (its `local.toml` is never rewritten by this amendment)
    // still shows its default row as a normal session, unchanged.
    let host: HostKey = "local".to_string();
    let mut anchor = ws_info("sot", "sot-be-sot");
    anchor.is_default = true;
    anchor.agent = "claude".to_string();
    anchor.runtime = "capsule".to_string();
    let list = vec![anchor];
    let kids = session_host_children(&host, &list);
    let kinds: Vec<&str> = kids.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, vec!["session_create", "session"]);
    assert!(kids.iter().any(|n| n.id.contains("sot-be-sot")));
}

#[test]
fn session_host_children_hides_a_default_tmux_row_with_no_agent_too() {
    // Owner ruling (2026-09-06, symmetry): a tmux backend's default row
    // with no agent is the inert anchor exactly like the Windows capsule
    // one -- a row that cannot be closed and invites an LLM pane it must
    // not have. Dev happens in a `ship-of-tools` row; the anchor is
    // never listed.
    let host: HostKey = "backend".to_string();
    let mut anchor = ws_info("sot", "sot-be-sot");
    anchor.is_default = true;
    anchor.agent = "none".to_string();
    anchor.runtime = "tmux".to_string();
    let list = vec![anchor];
    let kids = session_host_children(&host, &list);
    let kinds: Vec<&str> = kids.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, vec!["session_create"]);
}

#[test]
fn resplice_expanded_session_hosts_drops_a_row_no_longer_in_workspace_lists() {
    // fe-destroyed-row-refresh: `TreeView::set_root`'s same-root merge
    // preserves an already-expanded `session_host` row's OLD
    // descendants verbatim (`build_sessions_tree` only returns
    // level-1 host nodes), so a `workspace.list` refresh that drops a
    // destroyed workspace never reached the row on screen until the
    // host was manually collapsed and re-expanded. Proves the fix: an
    // EXPANDED host whose fresh list no longer carries a session loses
    // that row, while a surviving one stays.
    let host: HostKey = "local".to_string();
    let mut host_payload = serde_json::Map::new();
    host_payload.insert("host".to_string(), serde_json::Value::String(host.clone()));
    let mut view = TreeView::new();
    view.rows = vec![
        TreeRow {
            node: TreeNode {
                id: "sessions:".to_string(),
                label: "workspaces".to_string(),
                kind: "sessions".to_string(),
                has_children: true,
                badges: Vec::new(),
                payload: Default::default(),
            },
            depth: 0,
            expanded: true,
        },
        TreeRow {
            node: TreeNode {
                id: "sessions:host:local".to_string(),
                label: "local".to_string(),
                kind: "session_host".to_string(),
                has_children: true,
                badges: Vec::new(),
                payload: host_payload,
            },
            depth: 1,
            expanded: true, // already open — the destroy-a-row case requires it
        },
        TreeRow {
            node: State::build_session_row(&host, &ws_info("doomed", "sot-be-doomed")),
            depth: 2,
            expanded: false,
        },
    ];

    // The destroy already landed server-side: `local`'s fresh list no
    // longer has "doomed", but still has an unrelated survivor.
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert(host.clone(), vec![ws_info("survivor", "sot-be-survivor")]);

    resplice_expanded_session_hosts(&mut view, &lists);

    let session_ids: Vec<&str> = view
        .rows
        .iter()
        .filter(|r| r.node.kind == "session")
        .map(|r| r.node.id.as_str())
        .collect();
    assert!(
        !session_ids.iter().any(|id| id.contains("doomed")),
        "destroyed workspace's row must not survive a refresh of an expanded host: {session_ids:?}"
    );
    assert!(
        session_ids.iter().any(|id| id.contains("survivor")),
        "the surviving workspace's row must still be present: {session_ids:?}"
    );
}

// --- ADR 0046 decision 1: the declared-host label (display only) ---
//
// Manager review (S8, Codex finding B1): duplicate refusal was
// rejected here — no transport shutdown path exists to actually close
// a newcomer's connection, so `App::record_declared_host` (a plain
// `HashMap::insert`, no branch worth a dedicated unit test) feeds
// display only (Hosts mode, Sessions labels, the status line,
// connect/disconnect logs — see `host_label` below). The static
// same-port skip in `dial::resolve_connections` (restored) is what
// prevents two dials from ever reaching the same daemon.

#[test]
fn every_configured_host_shows_up_even_without_a_list_yet() {
    // ADR 0042 L2's acceptance: an unreachable (or still-mid-hello)
    // host is a VISIBLE node marked unreachable, not an absent one.
    // Two conns, one has answered workspace.list — both still get a
    // tree node; the empty one is flagged unreachable and childless.
    let ordered = vec!["alpha".to_string(), "beta".to_string()];
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert("alpha".to_string(), vec![ws_info("sot", "sot-be-sot")]);
    // "beta" has no list yet.
    let host_connected: HashMap<HostKey, bool> =
        [("alpha".to_string(), true)].into_iter().collect();

    let children: Vec<TreeNode> = ordered
        .iter()
        .map(|host| {
            let connected = host_connected.get(host).copied().unwrap_or(false);
            let has_list = lists.contains_key(host);
            host_tree_node(host, host, connected, has_list, host == "alpha")
        })
        .collect();

    assert_eq!(children.len(), 2, "every configured host gets a node");
    assert_eq!(children[0].id, "sessions:host:alpha");
    // alpha is connected AND active -- quiet status, "[current]" marker.
    assert_eq!(
        children[0].label,
        format!("{HOST_DIVIDER_GLYPH} alpha [current]")
    );
    assert!(children[0].has_children);

    assert_eq!(children[1].id, "sessions:host:beta");
    assert!(
        children[1].label.contains("[unreachable]"),
        "no list yet → unreachable marker, got {:?}",
        children[1].label
    );
    assert!(!children[1].has_children, "no list yet → no children");
}

#[test]
fn host_node_label_flips_live_with_connected_status_same_row_id() {
    // ADR 0042 L2a codex review, item L: on Connected/Disconnected the
    // tree is rebuilt so a node doesn't stay `unreachable` after
    // connecting or `connected` after dropping. `host_tree_node` is
    // the pure per-row seam that rebuild calls on every trigger (a
    // workspace.list reply, or now a bare Connected/Disconnected too)
    // — this pins that re-deriving the SAME row id with a flipped
    // `connected` flag flips the drawn status (Codex round, PR #172:
    // badges are gone, the label is the one source of truth), and
    // (crucially for `set_root`'s cursor/expansion preservation) the
    // id itself never changes across the flip.
    let before = host_tree_node(&"alpha".to_string(), "alpha", false, false, true);
    let after = host_tree_node(&"alpha".to_string(), "alpha", true, true, true);
    assert_eq!(before.id, after.id, "same host → same row id, always");
    assert!(before.label.contains("[unreachable]"));
    assert!(after.label.contains("[current]"));
    assert!(!after.label.contains("[unreachable]"));

    // And the reverse direction (a live Disconnected).
    let dropped = host_tree_node(&"alpha".to_string(), "alpha", false, true, true);
    assert_eq!(dropped.id, after.id);
    assert!(dropped.label.contains("[unreachable]"));
}

#[test]
fn host_tree_node_status_draws_in_the_label_not_just_the_unrendered_badges_vec() {
    // First live shakedown fix: nothing renders the tree's generic
    // `badges` vec (see `capsule_phase_tag`'s doc), so a healthy,
    // non-active host must say nothing extra, while `unreachable` and
    // `current` must show up as bracketed tags IN THE LABEL — the same
    // way every other status in this tree actually draws.
    let quiet = host_tree_node(&"alpha".to_string(), "alpha", true, true, false);
    assert_eq!(quiet.label, format!("{HOST_DIVIDER_GLYPH} alpha"));

    let unreachable = host_tree_node(&"alpha".to_string(), "alpha", false, true, false);
    assert_eq!(
        unreachable.label,
        format!("{HOST_DIVIDER_GLYPH} alpha [unreachable]")
    );

    let current = host_tree_node(&"alpha".to_string(), "alpha", true, true, true);
    assert_eq!(
        current.label,
        format!("{HOST_DIVIDER_GLYPH} alpha [current]")
    );

    let both = host_tree_node(&"alpha".to_string(), "alpha", false, true, true);
    assert_eq!(
        both.label,
        format!("{HOST_DIVIDER_GLYPH} alpha [unreachable] [current]")
    );
}
