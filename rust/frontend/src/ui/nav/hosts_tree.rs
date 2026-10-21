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
fn hosts_mode_row(name: &str, display: &str, link: Option<&str>, is_active: bool, is_default: bool) -> TreeNode {
    let status_word = link.unwrap_or("connected");
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

/// What a host's row says of its link when it is not connected (`None` when it is): the headline of the hello it
/// refused, the first line of its message up to the dash ("daemon out of date" for a daemon still running an older
/// protocol), `refused` for a refusal with no headline, else `unreachable`. A daemon that answered and refused is not
/// unreachable: that word hid the cause (a Windows field box, 2026-10-16: local read unreachable while an rc1 daemon
/// an update had left running refused the rc9.13 window every 5 s). Both host trees ask it.
pub(in crate::ui) fn host_link_status(connected: bool, refusal: Option<&str>) -> Option<&str> {
    if connected {
        return None;
    }
    Some(match refusal.and_then(|m| m.lines().next()) {
        Some(first) => first.split_once(" — ").map_or("refused", |(head, _)| head),
        None => "unreachable",
    })
}

/// ONE display projection for every host-keyed UI surface (ADR 0046
/// decision 1, S9/finding S14): the daemon's own
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
                let connected = self.hosts.host_connected.get(&name).copied().unwrap_or(false);
                let link = host_link_status(connected, self.hello_refused.get(&name).map(String::as_str));
                let is_active = name == self.active_host;
                let is_default = default_name == Some(name.as_str());
                let display = host_label(&self.hosts.declared_host, &name);
                hosts_mode_row(&name, display, link, is_active, is_default)
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
#[path = "hosts_tree_tests.rs"]
mod tests;
