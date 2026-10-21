use super::*;

#[test]
fn hosts_tree_refresh_keeps_the_cursor_on_the_host_it_was_on() {
    // Codex round, PR #172: `populate_hosts_tree` used to reposition
    // the cursor to the ACTIVE host on every call, including a live
    // refresh (a connect/disconnect while Hosts mode is active) --
    // stealing the cursor out from under a user looking at some OTHER
    // host. `set_root` re-seeding the SAME root id ("hosts:") already
    // re-anchors the previously-selected row by node id on its own;
    // a refresh should trust that instead of forcing a fresh position.
    let mut t = TreeView::new();
    t.set_root(
        node("hosts:", "hosts", true),
        vec![
            node("hosts:local", "local", false),
            node("hosts:beta", "beta", false),
        ],
    );
    t.selected = 2; // cursor on "beta", not the active host
                    // A refresh re-seeds the same root -- no cursor repositioning,
                    // just the id-based reanchor `set_root` already does.
    t.set_root(
        node("hosts:", "hosts", true),
        vec![
            node("hosts:local", "local", false),
            node("hosts:beta", "beta", false),
        ],
    );
    assert_eq!(t.rows[t.selected].node.id, "hosts:beta");
}

#[test]
fn expand_hosts_root_repopulates_after_collapse_then_reexpand() {
    // Mode::Hosts's own bug (first live shakedown): its root's children
    // used to be built only on mode ENTRY (`populate_hosts_tree` ->
    // `set_root`); collapsing then re-expanding the root without
    // leaving the mode took the generic `TreeChildren` wire path,
    // which has no server-side handler for the synthetic "hosts:" id
    // and left the root empty. Goes through the actual guarded
    // function (Codex round, PR #172) rather than a hand-flipped
    // `expanded` -- proves the real dispatch path, not just
    // `apply_children`'s general mechanics (already covered by
    // `apply_children_ignores_reply_for_collapsed_parent`).
    let mut t = TreeView::new();
    t.set_root(
        node("hosts:", "hosts", true),
        vec![node("hosts:local", "local", false)],
    );
    t.selected = 0;
    assert!(t.collapse_selected());
    assert_eq!(t.rows.len(), 1);
    assert!(!t.rows[0].expanded);

    let fresh = vec![
        node("hosts:local", "local", false),
        node("hosts:beta", "beta", false),
    ];
    assert!(expand_hosts_root(&mut t, Mode::Hosts, fresh));
    assert_eq!(
        t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
        vec!["hosts:", "hosts:local", "hosts:beta"]
    );
    assert!(t.rows[0].expanded);
}

#[test]
fn expand_hosts_root_declines_outside_mode_hosts_or_off_the_root_row() {
    // Codex round, PR #172: the guard checks BOTH `mode == Mode::Hosts`
    // and the exact root id "hosts:" -- not just a row's `kind`, which
    // can't rule out some other mode's tree incidentally carrying a
    // "hosts:"-shaped row, or the cursor sitting on a CHILD instead of
    // the root.
    let mut wrong_mode = TreeView::new();
    wrong_mode.set_root(
        node("hosts:", "hosts", true),
        vec![node("hosts:local", "local", false)],
    );
    wrong_mode.selected = 0;
    assert!(wrong_mode.collapse_selected());
    assert!(!expand_hosts_root(
        &mut wrong_mode,
        Mode::Sessions,
        vec![node("hosts:local", "local", false)],
    ));
    assert_eq!(wrong_mode.rows.len(), 1, "declined -- stays collapsed");
    assert!(!wrong_mode.rows[0].expanded);

    let mut off_root = TreeView::new();
    off_root.set_root(
        node("hosts:", "hosts", true),
        vec![node("hosts:local", "local", false)],
    );
    off_root.selected = 1; // on the child row, not the root
    assert!(!expand_hosts_root(
        &mut off_root,
        Mode::Hosts,
        vec![node("hosts:local", "local", false)],
    ));
}

#[test]
fn hosts_mode_row_label_and_id_come_only_from_the_caller() {
    // Mode::Hosts's own per-row builder (first live shakedown fix;
    // Codex round, PR #172 dropped the endpoint text and `badges` --
    // `[current]`/`[default]` are baked into the label instead, the
    // one source of truth since nothing reads `badges`):
    // membership/connected/current/default are all caller-supplied --
    // this row-builder itself never touches the connection registry, matching
    // `hosts_tree_children`'s contract that `local` (no --dial
    // entry) gets a row exactly like any configured host.
    let row = hosts_mode_row("local", "local", None, true, false);
    assert_eq!(row.id, "hosts:local");
    assert_eq!(row.kind, "host");
    assert!(!row.has_children);
    assert_eq!(row.label, "local · connected [current]");
    assert!(row.badges.is_empty());

    let unreachable_default = hosts_mode_row("beta", "beta", Some("unreachable"), false, true);
    assert_eq!(unreachable_default.label, "beta · unreachable [default]");
    assert!(unreachable_default.badges.is_empty());

    let quiet = hosts_mode_row("gamma", "gamma", None, false, false);
    assert_eq!(quiet.label, "gamma · connected");
}

#[test]
fn hosts_mode_row_displays_whatever_the_caller_resolved_via_host_label() {
    // ADR 0046 decision 1, S9: hosts_mode_row itself
    // has no opinion on dial-vs-declared -- it displays exactly the
    // string the caller resolved via `host_label`.
    let differs = hosts_mode_row("myserver", "realhost", None, false, false);
    assert_eq!(differs.label, "realhost · connected");
}

#[test]
fn a_host_that_refused_the_hello_is_named_by_the_refusal_not_unreachable() {
    // The head of the body `protocol_mismatch_message` builds for an older daemon (net/transport/hello.rs).
    let skew = "daemon out of date — daemon 0.6.6-rc1 speaks protocol 2, this frontend 0.6.6-rc9.13 speaks 3\n\n\
                    backend: ...";
    assert_eq!(host_link_status(false, Some(skew)), Some("daemon out of date"));
    assert_eq!(host_link_status(false, Some("host x has said hello as two OS accounts")), Some("refused"));
    assert_eq!(host_link_status(false, None), Some("unreachable"));
    assert_eq!(host_link_status(true, Some(skew)), None);
    let row = hosts_mode_row("local", "local", host_link_status(false, Some(skew)), true, true);
    assert_eq!(row.label, "local · daemon out of date [default] [current]");
}

#[test]
fn host_label_falls_back_to_the_dial_key_when_undeclared() {
    let declared_host: HashMap<HostKey, String> = HashMap::new();
    assert_eq!(host_label(&declared_host, &"myserver".to_string()), "myserver");
}

#[test]
fn host_label_prefers_the_declaration_when_present() {
    let mut declared_host: HashMap<HostKey, String> = HashMap::new();
    declared_host.insert("myserver".to_string(), "realhost".to_string());
    assert_eq!(host_label(&declared_host, &"myserver".to_string()), "realhost");
}
