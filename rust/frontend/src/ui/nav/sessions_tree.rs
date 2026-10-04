//! The Sessions tree: host nodes, session rows and their agent tone.

use super::*;

/// One host's `session_host` node in the Sessions tree (ADR 0042 L2a) —
/// pure, no `State` dependency, so `build_sessions_tree`'s "every configured
/// host shows up, connected or not" behavior is directly unit-testable.
/// `has_list` (not `connected`) drives `has_children`: a host can be
/// connected but simply not have answered `workspace.list` yet, and either
/// way there's nothing to expand into until it has. `host` stays the
/// routing key (id, payload) — `display` (manager review S9: resolved by
/// the caller via `host_label`, the ONE display projection) is what
/// actually appears in the label text.
///
/// First live shakedown fix: `badges` alone never reached the screen —
/// nothing renders the tree's generic `badges` vec (see `capsule_phase_tag`'s
/// own doc on the ONE place a status phase is surfaced). Every OTHER
/// visible status in this tree is baked straight into `label` text
/// (`build_session_row`'s `capsule_phase_tag` prefix); this bakes host
/// status in the same way, as a bracketed tag, so it draws the way every
/// other badge here actually does. Connected, non-current hosts stay
/// quiet — nothing to shout about — so a healthy multi-host list doesn't
/// drown in tags; `unreachable` and `current` always show. `badges` itself
/// is deleted (Codex round, PR #172): nothing ever read it, and computing
/// it alongside the SAME status this label already carries was pure
/// duplication — the label is the one source of truth now.
fn host_tree_node(host: &HostKey, display: &str, connected: bool, has_list: bool, is_active: bool) -> TreeNode {
    let mut tags = Vec::new();
    if !connected {
        tags.push("[unreachable]");
    }
    if is_active {
        tags.push("[current]");
    }
    let label = if tags.is_empty() {
        format!("{HOST_DIVIDER_GLYPH} {display}")
    } else {
        format!("{HOST_DIVIDER_GLYPH} {display} {}", tags.join(" "))
    };
    let mut payload = serde_json::Map::new();
    payload.insert("host".to_string(), serde_json::Value::String(host.clone()));
    TreeNode {
        id: format!("sessions:host:{host}"),
        label,
        kind: "session_host".to_string(),
        has_children: has_list,
        badges: Vec::new(),
        payload,
    }
}

/// A `session_host` row's children — `[+ create new]` then that host's
/// workspace rows — built from one host's slice of `workspace_lists`. Pure
/// / no `State` dependency beyond `State::build_session_row` itself
/// (already pure). Shared by `try_expand_session_host_local` (first
/// expand) and `resplice_expanded_session_hosts` (re-expand after a
/// refresh) so both splice identical rows from identical data.
///
/// 2026-09-04 amendment (owner ruling): a host's DEFAULT CAPSULE row
/// with `agent == "none"` is the daemon's inert anchor workspace, not a
/// session — it never appears here at all, in either kind or shape.
/// `workspace.list` still returns it (`is_default: true`, needed so the
/// daemon still has a fallback workspace and the FE's own
/// `default_workspace_slug` bookkeeping still resolves), but this is the
/// ONE place that turns that list into visible TREE rows, so filtering
/// here is enough to keep it off the Sessions tree (the `[+ create new]`
/// row and the user's own sessions are all that remain). The bottom
/// session STRIP is a separate list (`fresh_workspace_caches`, below) —
/// same amendment applies there via the same
/// `WorkspaceInfo::is_inert_anchor` predicate so the two never drift.
///
/// No runtime leg (owner ruling 2026-09-06, symmetry): a tmux backend's
/// default row with no agent is the anchor too and is hidden the same way;
/// Ship of Tools development runs in a `ship-of-tools` row like any other
/// project, and the SoT LLM lives in the drawer. A
/// default row that still carries a real agent (an existing capsule box
/// before its own converge to this amendment — `local.toml` is never
/// rewritten) is untouched: still a normal, visible session row.
fn session_host_children(
    host: &HostKey,
    list: &[crate::transport::WorkspaceInfo],
) -> Vec<TreeNode> {
    let mut payload = serde_json::Map::new();
    payload.insert("host".to_string(), serde_json::Value::String(host.clone()));
    let create_row = TreeNode {
        id: format!("sessions:{host}:+create"),
        label: "[+ create new]".to_string(),
        kind: "session_create".to_string(),
        has_children: false,
        badges: Vec::new(),
        payload,
    };
    let mut kids = vec![create_row];
    kids.extend(
        list.iter()
            .filter(|w| !w.is_inert_anchor())
            .map(|w| State::build_session_row(host, w)),
    );
    kids
}

/// Re-splice the children of every currently-expanded `session_host` row in
/// `view` from `workspace_lists` (`session_host_children`, above) — the
/// SAME local, no-round-trip construction `try_expand_session_host_local`
/// runs on first expand, just re-run for whichever hosts are already open.
///
/// Needed because `TreeView::set_root`'s same-root merge (its own doc)
/// preserves an already-expanded child's OLD descendants verbatim:
/// `build_sessions_tree` only returns level-1 host nodes, so the session
/// rows underneath an open host are never carried by a `workspace.list`
/// reply — they're spliced in locally, once, at expand time. Left
/// unpatched, a destroy/create/status-flip landing while the host is
/// expanded (the common case: selecting a row to destroy REQUIRES its
/// host open) never reaches the rows actually on screen — a destroyed
/// workspace's row stayed visible until the host node was manually
/// collapsed and re-expanded (fe-destroyed-row-refresh, 2026-09-03). A
/// host missing from `workspace_lists` is left alone — nothing fresh to
/// splice.
fn resplice_expanded_session_hosts(
    view: &mut TreeView,
    workspace_lists: &HashMap<HostKey, Vec<crate::transport::WorkspaceInfo>>,
) {
    let open: Vec<(String, HostKey)> = view
        .rows
        .iter()
        .filter(|r| r.node.kind == "session_host" && r.expanded)
        .filter_map(|r| {
            let host = r.node.payload.get("host")?.as_str()?.to_string();
            Some((r.node.id.clone(), host))
        })
        .collect();
    for (parent_id, host) in open {
        if let Some(list) = workspace_lists.get(&host) {
            view.apply_children(&parent_id, session_host_children(&host, list));
        }
    }
}

/// ADR 0042 slice L1b: the capsule row's supervisor phase (ADR 0041
/// Lifecycle, snake_case, plus `"unreachable"` and, since ADR 0030 §8
/// decision 31c, `"foreign"`), folded into the Sessions-row glance line —
/// nothing renders the tree's `badges` vec today (deletion pressure: a
/// second, invisible `badges` entry would name no observable invariant).
/// `capsule_row_is_foreign` below is the SECOND surface this same phase
/// value drives — the row's colour, not only this text tag.
fn capsule_phase_tag(phase: &str) -> String {
    format!("[{phase}]")
}

/// True when a Sessions row's capsule supervisor lane refused this
/// daemon's build (`WorkspaceListEntry.phase == "foreign"`, ADR 0030 §8
/// decision 31c) — pulled out so the nav-row colour branch is
/// unit-tested without a live `State`. Only `kind == "session"` rows
/// carry a `phase` payload key at all (`build_session_row`); every other
/// kind is never foreign, regardless of what `phase` happens to hold.
pub(in crate::ui) fn capsule_row_is_foreign(kind: &str, phase: Option<&str>) -> bool {
    kind == "session" && phase == Some("foreign")
}

impl State {
    /// Build one `session`-kind `TreeNode` row for workspace `w` on `host` —
    /// the per-row payload/badges/glance-line logic, unchanged from
    /// pre-L2a except the row id and payload now carry `host` (ADR 0042
    /// L2a: two hosts can each report a `sot-be-sot` tmux session, so the
    /// row id must be host-qualified even though the `name` payload stays
    /// the bare session_name `attach_session_to_bl`/`selected_session_name`
    /// already key off).
    fn build_session_row(host: &HostKey, w: &crate::transport::WorkspaceInfo) -> TreeNode {
        let mut payload = serde_json::Map::new();
        // `name` stays the tmux session name so the existing
        // `selected_session_name` / `attach_session_to_bl` paths work
        // unchanged — routing to the right daemon is `host`'s job now.
        payload.insert(
            "name".to_string(),
            serde_json::Value::String(w.session_name.clone()),
        );
        payload.insert("host".to_string(), serde_json::Value::String(host.clone()));
        payload.insert(
            "workspace_id".to_string(),
            serde_json::Value::String(w.workspace_id.clone()),
        );
        payload.insert(
            "slug".to_string(),
            serde_json::Value::String(w.slug.clone()),
        );
        payload.insert(
            "label".to_string(),
            serde_json::Value::String(w.label.clone()),
        );
        payload.insert(
            "project_root".to_string(),
            serde_json::Value::String(w.project_root.clone()),
        );
        payload.insert(
            "kernel_running".to_string(),
            serde_json::Value::Bool(w.kernel_running),
        );
        payload.insert(
            "is_default".to_string(),
            serde_json::Value::Bool(w.is_default),
        );
        payload.insert(
            "agent_state".to_string(),
            serde_json::Value::String(w.agent_state.clone()),
        );
        payload.insert(
            "agent_status_at".to_string(),
            serde_json::Value::String(w.agent_status_at.clone()),
        );
        // ADR 0030 §8 decision 31c: the capsule's supervisor-lane phase —
        // `""` for a tmux row or a daemon that predates the field, so the
        // NavRow foreign check below reads it the same as an absent value.
        // `capsule_phase_tag` below already bakes this into the glance
        // TEXT (`w.phase.as_deref()`, unconditional since ADR 0045
        // decision 1); this payload copy is what lets the (already
        // host-agnostic) row-colour pass see it too.
        payload.insert(
            "phase".to_string(),
            serde_json::Value::String(w.phase.clone().unwrap_or_default()),
        );
        let mut badges = Vec::new();
        if w.is_default {
            badges.push("default".to_string());
        }
        if w.kernel_running {
            badges.push("kernel".to_string());
        }
        if w.repl_state == "starting" {
            badges.push("repl_starting".to_string());
        }
        // SHOULD-FIX (Codex review, lane B5 discharge): unconditional —
        // a capsule row attaches through its own daemon on every
        // platform now (ADR 0045 decision 1), so this glance is as
        // meaningful off Windows as on it.
        let capsule_phase = (w.runtime == "capsule")
            .then_some(w.phase.as_deref())
            .flatten();
        let glance_base = if !w.agent_summary.is_empty() {
            w.agent_summary.clone()
        } else if !w.agent_state.is_empty() {
            w.agent_state.clone()
        } else {
            w.project_root.clone()
        };
        let glance = if w.repl_state == "starting" {
            format!("julia starting (precompiling)… · {glance_base}")
        } else {
            glance_base
        };
        let glance = match capsule_phase {
            Some(phase) => format!("{} {glance}", capsule_phase_tag(phase)),
            None => glance,
        };
        let label = if w.label.is_empty() {
            w.slug.clone()
        } else {
            format!("{} · {}", w.label, glance)
        };
        // Per-session accounts (owner-simplified brief, 2026-09-15): a
        // short suffix naming which subscription this row spends — shown
        // ONLY when it isn't the agent's default directory (`w.account`
        // empty), so the common case renders exactly as before.
        let label = if w.account.is_empty() {
            label
        } else {
            format!("{label} · {}", w.account)
        };
        TreeNode {
            id: format!("sessions:{host}:{}", w.session_name),
            label,
            kind: "session".to_string(),
            has_children: false,
            badges,
            payload,
        }
    }

    /// Build the host-grouped Sessions tree (ADR 0042 L2a): root → one
    /// `session_host` node per connected/unreachable host (in
    /// `ordered_hosts` order — local first, then --dial argument order), each
    /// carrying a status badge and the `HOST_DIVIDER_GLYPH` prefix. A host
    /// node's own children — its `[+ create new]` row and that host's
    /// session rows — are spliced in locally by `try_expand_selected`'s
    /// `session_host` arm (the data is already here from `workspace_lists`;
    /// no wire round-trip needed to expand one). An unreachable host still
    /// gets a node with its last-known rows — it doesn't vanish.
    fn build_sessions_tree(&self) -> (TreeNode, Vec<TreeNode>) {
        let root = TreeNode {
            id: "sessions:".to_string(),
            label: "workspaces".to_string(),
            kind: "sessions".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        let children = self
            .ordered_hosts()
            .into_iter()
            .map(|host| {
                let connected = self.host_connected.get(&host).copied().unwrap_or(false);
                let has_list = self.workspace_lists.contains_key(&host);
                let is_active = host == self.active_host;
                let display = host_label(&self.declared_host, &host);
                host_tree_node(&host, display, connected, has_list, is_active)
            })
            .collect();
        (root, children)
    }

    /// Local (no wire round-trip) expansion of a `session_host` row: its
    /// `[+ create new]` row plus that host's session rows, built from
    /// `workspace_lists` — the same data `build_sessions_tree` already
    /// pulled from. Returns `false` (declines) for any other row kind so
    /// `try_expand_selected` falls through to its normal wire-request path.
    pub(in crate::ui) fn try_expand_session_host_local(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if row.node.kind != "session_host" || row.expanded {
            return false;
        }
        let Some(host) = row
            .node
            .payload
            .get("host")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return false;
        };
        // A host with no list yet has no children (matches the tree's own
        // `has_children: false` for that state, ADR 0042 L2's acceptance:
        // still a visible node, just nothing to expand into).
        let Some(list) = self.workspace_lists.get(&host) else {
            return false;
        };
        let parent_id = row.node.id.clone();
        let kids = session_host_children(&host, list);
        // `apply_children` requires the parent already marked expanded
        // (it drops a splice for a collapsed parent — the collapse-race
        // fix); mark it BEFORE splicing, same order every wire-based
        // expand uses at request time.
        if let Some(r) = self.tree.rows.get_mut(self.tree.selected) {
            r.expanded = true;
        }
        self.tree.apply_children(&parent_id, kids);
        self.window.request_redraw();
        true
    }

    /// Sessions-mode (B4): finalize the typed label. Derives the tmux
    /// session name + project directory via the same slug rule the
    /// backend uses (paths::slug) so a `--label foo` daemon and the
    /// frontend agree on the name. `cwd = $SOT_PROJECTS_ROOT/<label>`
    /// (defaults to `$HOME/julia_dev/<label>` on Linux).
    /// Sessions-mode (ADR 0013): if the selected row is a session or a
    /// pane, return the session name (panes have a `session` payload).
    /// `None` for any other row kind.
    pub(in crate::ui) fn selected_session_name(&self) -> Option<String> {
        let row = self.tree.rows.get(self.tree.selected)?;
        if row.node.kind == "session" {
            return row
                .node
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        if row.node.kind == "pane" {
            return row
                .node
                .payload
                .get("session")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        None
    }

    /// The cursored `session`/`pane` row's OWN host (ADR 0042 L2a). Both
    /// row kinds carry `payload.host` — `session` rows from
    /// `build_session_row`, `pane` rows from the `TmuxPanes` reply handler
    /// (stamped from that reply's own `event_host`, since two hosts can
    /// both report a `sot-be-sot` session and matching by NAME alone would
    /// pick whichever host happens to have one, silently wrong). `None`
    /// for any other row kind or a row from a daemon old enough not to
    /// send the field.
    pub(in crate::ui) fn selected_session_host(&self) -> Option<HostKey> {
        let row = self.tree.rows.get(self.tree.selected)?;
        if row.node.kind != "session" && row.node.kind != "pane" {
            return None;
        }
        row.node
            .payload
            .get("host")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Rebuild the host-grouped Sessions tree from current state
    /// (`workspace_lists` + `host_connected`) and either install it into
    /// the active view or park it — the ONE seam every trigger for a
    /// Sessions-tree refresh shares (a `workspace.list` reply landing, or
    /// ADR 0042 L2a codex review item L: a `Connected`/`Disconnected` on
    /// ANY host, so a node doesn't stay `unreachable` after connecting or
    /// `connected` after dropping until the next unrelated reply happens
    /// to land). `set_root` with the SAME root id ("sessions:") preserves
    /// expansion/cursor for rows that still exist — but it also preserves
    /// an already-expanded `session_host` row's OLD descendants verbatim
    /// (`build_sessions_tree` only returns level-1 host nodes), so
    /// `resplice_expanded_session_hosts` re-derives any open host's rows
    /// from the fresh `workspace_lists` right after — see its own doc
    /// (fe-destroyed-row-refresh, 2026-09-03).
    pub(in crate::ui) fn rebuild_and_install_sessions_tree(&mut self) {
        let (root, children) = self.build_sessions_tree();
        let reply_key: TreeKey = (Mode::Sessions, TreeScope::Global);
        if reply_key != self.active_tree_key() {
            // Reply-over-reply allowed (see the TreeRoot park
            // rationale); user-stashed state blocks.
            let slot = self.tree_store.slot_mut(reply_key);
            if slot.view.rows.is_empty() || slot.from_reply {
                slot.view.set_root(root, children);
                resplice_expanded_session_hosts(&mut slot.view, &self.workspace_lists);
                slot.from_reply = true;
            } else {
                tracing::debug!(
                    "sessions-tree refresh dropped — parked Sessions slot holds user state"
                );
            }
            return;
        }
        self.tree.set_root(root, children);
        resplice_expanded_session_hosts(&mut self.tree, &self.workspace_lists);
        if let Some(n) = self.pending_initial_selection.take() {
            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
        }
    }
}

/// Resolve a Sessions row's agent state from its node payload into the render
/// tone plus a wilt flag (true = active state gone stale). `None` when there
/// is no agent state to show, so the row renders exactly as it did before
/// state-nav. `now` is injected so the staleness check stays unit-testable.
pub(in crate::ui) fn agent_tone_for(
    payload: &serde_json::Map<String, serde_json::Value>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(AgentTone, bool)> {
    let state = payload
        .get("agent_state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status_at = payload
        .get("agent_status_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    agent_tone_from(state, status_at, now)
}

/// Core state→tone + staleness logic on the raw registry fields, shared by the
/// Sessions-mode rows (`agent_tone_for`, payload-keyed) and the bottom session
/// strip (slug-keyed `workspace_states`). `None` for an empty/unknown state so
/// the caller renders exactly as it did before state-nav. `now` is injected so
/// the wilt check stays unit-testable.
pub(in crate::ui) fn agent_tone_from(
    state: &str,
    status_at: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(AgentTone, bool)> {
    let tone = match state {
        "working" => AgentTone::Working,
        "idle" => AgentTone::Idle,
        "waiting" => AgentTone::Waiting,
        "blocked" => AgentTone::Blocked,
        "done" => AgentTone::Done,
        _ => return None,
    };
    let aged = tone == AgentTone::Working
        && (!status_at.is_empty())
            .then(|| chrono::DateTime::parse_from_rfc3339(status_at).ok())
            .flatten()
            .map(|t| {
                now.signed_duration_since(t.with_timezone(&chrono::Utc))
                    > chrono::Duration::minutes(AGENT_STALE_MINUTES)
            })
            .unwrap_or(false);
    Some((tone, aged))
}

#[cfg(test)]
#[path = "sessions_tree_tests.rs"]
mod tests;
