//! The Hosts tree: one row per host and the selection of the active host.

use super::*;

/// One host's `host`-kind row for Mode::Hosts's flat list (first live
/// shakedown fix) — pure, no `State` dependency, mirroring `host_tree_node`
/// above: every LIVE connection gets a row (membership + `connected`/
/// `current`/`default` all come from the caller's already-resolved
/// connection set). `current`/`default` are baked into the label as
/// bracketed tags, the same way `host_tree_node` bakes its own status
/// (Codex round, PR #172) — `badges` is deleted for the same reason: it's
/// unread, and duplicated exactly what the label already says.
///
/// `declared` (ADR 0046 decision 1, revised): the daemon's own declared
/// identity for this dial (`App::declared_host`), shown alongside `name`
/// whenever it differs — `name` (the `--dial` host name) is never
/// replaced by it, only annotated, so the row still says what
/// the user actually configured.
fn hosts_mode_row(name: &str, display: &str, connected: bool, is_active: bool, is_default: bool) -> TreeNode {
    let status_word = if connected {
        "connected"
    } else {
        "unreachable"
    };
    let mut tags = Vec::new();
    if is_default {
        tags.push("[default]");
    }
    if is_active {
        tags.push("[current]");
    }
    let label = if tags.is_empty() {
        format!("{display} · {status_word}")
    } else {
        format!("{display} · {status_word} {}", tags.join(" "))
    };
    let mut payload = serde_json::Map::new();
    payload.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    TreeNode {
        id: format!("hosts:{name}"),
        label,
        kind: "host".to_string(),
        has_children: false,
        badges: Vec::new(),
        payload,
    }
}

/// ONE display projection for every host-keyed UI surface (ADR 0046
/// decision 1, manager review S9/finding S14): the daemon's own
/// declaration for `dial` if it has reported one, else `dial` itself (the
/// `--dial` host name). Display only — routing keys (`conns`,
/// `host_connected`, and every other host-keyed map) are never re-homed
/// and never read through this. Used by Hosts mode rows, Sessions
/// labels, the status line, and connect/disconnect log lines — the one
/// place any of them decides what a host is CALLED on screen; no
/// separate truncation or composite display anywhere else.
pub(in crate::ui) fn host_label<'a>(declared_host: &'a HashMap<HostKey, String>, dial: &'a HostKey) -> &'a str {
    declared_host.get(dial).map(String::as_str).unwrap_or(dial.as_str())
}

/// Guard + mutation core of `try_expand_hosts_root_local` (Codex round,
/// PR #172): requires BOTH `mode == Mode::Hosts` and the exact root id
/// `"hosts:"` (depth zero) — `kind == "hosts"` alone doesn't rule out
/// some other row incidentally carrying that kind string, and doesn't
/// rule out firing while a DIFFERENT mode's tree happens to be showing.
/// Declines (returns `false`) for any other row, an already-expanded
/// root, or the wrong mode. No `State` dependency beyond the tree itself
/// and the freshly-built `children`, so this is directly unit-testable —
/// `try_expand_selected`'s own dispatch, minus the redraw.
///
/// Marks the row `expanded` at REQUEST time, then splices via
/// `apply_children` — never `populate_hosts_tree`'s own `set_root`, which
/// treats a currently-collapsed same-id root as "the user closed this,
/// keep it collapsed" and would silently no-op here (the same contract
/// `try_expand_session_host_local` relies on for `session_host`).
fn expand_hosts_root(tree: &mut TreeView, mode: Mode, children: Vec<TreeNode>) -> bool {
    if mode != Mode::Hosts {
        return false;
    }
    let Some(row) = tree.rows.get(tree.selected) else {
        return false;
    };
    if row.node.id != "hosts:" || row.expanded {
        return false;
    }
    if let Some(r) = tree.rows.get_mut(tree.selected) {
        r.expanded = true;
    }
    tree.apply_children("hosts:", children);
    true
}

impl State {
    /// Rebuild the nav tree from `conns` + `host_connected`. Called on
    /// entering `Mode::Hosts`. Each LIVE connection — `local` (implicit,
    /// ADR 0042 L2b) plus every `[host.<name>]` section that resolved to
    /// one — becomes one row showing live connected/unreachable status.
    ///
    /// ADR 0042 L2a: this was ADR 0015's "pick one, Ctrl+Q + relaunch"
    /// single-host switch (superseded — see that ADR's note); every
    /// configured host with a reachable endpoint is now a LIVE, permanent
    /// connection, so there is nothing left to "pick" — the mode became a
    /// live status list. `pick_host_under_cursor` (Enter) moves the
    /// Sessions-mode cursor to the host's node instead of persisting a
    /// launcher target.
    ///
    /// First live shakedown fix: the OLD version iterated the host registry
    /// directly, so `local` — which needs no config entry at all (ADR 0042
    /// L2b) — never got a row. Iterating `ordered_hosts()` (== `conns`,
    /// local-first display order — the SAME source `build_sessions_tree`
    /// already uses for its host-grouped rows) is the one source of truth
    /// now; topology plan (lane D) deleted the `hosts.toml` registry
    /// entirely, so there is no second list left to agree with. A per-row
    /// endpoint string used to live here too — deleted (Codex round, PR
    /// #172): it re-read the (now-gone) registry and could disagree with
    /// the endpoint the connection actually resolved to (a CLI override,
    /// or the CLI-only synthesized host) — the row is name + status +
    /// markers now; the endpoint was decoration that could lie.
    pub(in crate::ui) fn populate_hosts_tree(&mut self) {
        let root = TreeNode {
            id: "hosts:".to_string(),
            label: "hosts".to_string(),
            kind: "hosts".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        let children = self.hosts_tree_children();
        // A LIVE refresh (connect/disconnect while Hosts mode is active)
        // must not move the cursor out from under the user (Codex round,
        // PR #172) -- `set_root` re-seeding the SAME root id already
        // re-anchors the previously-selected row by node id on its own.
        // Landing on the active host at all is `select_active_host`'s job,
        // called only for a genuinely FIRST population (mode entry, a
        // first-visited workspace, resume).
        self.tree.set_root(root, children);
        self.window.request_redraw();
    }

    /// Point the Hosts-mode cursor at the active host -- called only when
    /// the view is being newly populated (mode entry, a first-visited
    /// workspace, resume), never after `populate_hosts_tree` alone on a
    /// live refresh: `set_root`'s own id-based reanchor already keeps a
    /// refresh's cursor where the user left it (Codex round, PR #172).
    /// Codex review (PR #163): the OLD version computed a position into
    /// `hosts_config.hosts` (0 = first host) and used it directly as
    /// `self.tree.selected` -- off by one, since row 0 of the rendered
    /// tree is the ROOT ("hosts:") and the first host lands at row 1.
    /// Restoring by NODE ID against the actual post-`set_root` rows
    /// sidesteps that arithmetic entirely rather than just patching the
    /// +1.
    pub(in crate::ui) fn select_active_host(&mut self) {
        let want_id = format!("hosts:{}", self.active_host);
        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == want_id) {
            self.tree.selected = idx;
        }
    }

    /// The Hosts-mode root's children, in `ordered_hosts()` (local-first)
    /// display order — the pure per-call rebuild `populate_hosts_tree` and
    /// `try_expand_hosts_root_local` both delegate to, so a mode-entry
    /// refresh and a root re-expand can never drift apart. The `default`
    /// badge marks whichever host `resolve_default_host` would fall back to
    /// (`conns.first()` — there is no more configured default since
    /// topology plan/`--dial`, lane D) — membership and
    /// connected/unreachable/current status come from
    /// `conns`/`host_connected`, which is the only way `local` (no config
    /// file required, ADR 0042 L2b) gets a row at all.
    fn hosts_tree_children(&self) -> Vec<TreeNode> {
        let default_name = self.conns.first().map(|(h, _)| h.as_str());
        self.ordered_hosts()
            .into_iter()
            .map(|name| {
                let connected = self.host_connected.get(&name).copied().unwrap_or(false);
                let is_active = name == self.active_host;
                let is_default = default_name == Some(name.as_str());
                let display = host_label(&self.declared_host, &name);
                hosts_mode_row(&name, display, connected, is_active, is_default)
            })
            .collect()
    }

    /// Mode::Hosts Enter handler (ADR 0042 L2a) — moves the Sessions-mode
    /// cursor to the picked host's group node. Replaces ADR 0015's
    /// "persist `last_host` + Ctrl+Q + relaunch" (deleted; see that ADR's
    /// superseded note): every host is already a live connection, so
    /// there's nothing to relaunch into — Enter just navigates there.
    pub(in crate::ui) fn pick_host_under_cursor(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        if row.node.kind != "host" {
            return;
        }
        let Some(name) = row
            .node
            .payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return;
        };
        self.enter_mode(Mode::Sessions);
        // Rebuild the Sessions tree synchronously from whatever
        // `workspace_lists` already holds, BEFORE searching (Codex round,
        // PR #172): `enter_mode`'s own Sessions arm only fans out wire
        // requests, so a parked or never-built tree would have no
        // `sessions:host:<name>` row to find below even when the data is
        // already cached locally and no round trip is actually needed.
        // Genuinely missing data (first-ever fetch still in flight) still
        // lands degraded below -- the cursor simply stays put.
        self.rebuild_and_install_sessions_tree();
        let host_row_id = format!("sessions:host:{name}");
        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == host_row_id) {
            self.tree.selected = idx;
        }
        self.status = format!("host · {name}");
        self.window.request_redraw();
    }

    /// Local (no wire round-trip) re-expansion of the Mode::Hosts ROOT row
    /// (first live shakedown fix): its children come from `conns` +
    /// `host_connected`, already held in memory (`hosts_tree_children`,
    /// the same builder `populate_hosts_tree` uses on mode entry) — no
    /// server round trip needed to rebuild them. The guard + mutation
    /// itself is `expand_hosts_root`, a pure free function (Codex round,
    /// PR #172) — see its own doc for why it checks BOTH `mode` and the
    /// exact root id, not just a row's `kind`.
    pub(in crate::ui) fn try_expand_hosts_root_local(&mut self) -> bool {
        let children = self.hosts_tree_children();
        if !expand_hosts_root(&mut self.tree, self.mode, children) {
            return false;
        }
        self.window.request_redraw();
        true
    }
}

#[cfg(test)]
mod tests {
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
        let row = hosts_mode_row("local", "local", true, true, false);
        assert_eq!(row.id, "hosts:local");
        assert_eq!(row.kind, "host");
        assert!(!row.has_children);
        assert_eq!(row.label, "local · connected [current]");
        assert!(row.badges.is_empty());

        let unreachable_default = hosts_mode_row("beta", "beta", false, false, true);
        assert_eq!(unreachable_default.label, "beta · unreachable [default]");
        assert!(unreachable_default.badges.is_empty());

        let quiet = hosts_mode_row("gamma", "gamma", true, false, false);
        assert_eq!(quiet.label, "gamma · connected");
    }

    #[test]
    fn hosts_mode_row_displays_whatever_the_caller_resolved_via_host_label() {
        // ADR 0046 decision 1, manager review S9: hosts_mode_row itself
        // has no opinion on dial-vs-declared -- it displays exactly the
        // string the caller resolved via `host_label`.
        let differs = hosts_mode_row("myserver", "realhost", true, false, false);
        assert_eq!(differs.label, "realhost · connected");
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
}
