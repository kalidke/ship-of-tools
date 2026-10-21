//! The projection of each daemon's workspace.list into the strip: declared sessions, the union caches, the activity order, connection status.

use super::*;

/// Output of `fresh_workspace_caches` — everything `State::rebuild_workspace_caches`
/// applies onto `self` in one pass, minus the one thing that needs history
/// (flash-on-transition, which stays in the `State` method since it reads
/// the PRIOR `prev_workspace_states`).
struct FreshWorkspaceCaches {
    workspace_slugs: Vec<WsKey>,
    workspace_labels: HashMap<WsKey, String>,
    workspace_project_roots: HashMap<WsKey, String>,
    workspace_states: HashMap<WsKey, (String, String)>,
    workspace_id_slugs: HashMap<(HostKey, String), WsKey>,
    /// Only the entries an old-daemon-safe insert would touch (non-empty
    /// `repl_state`) — `repl_lifecycle` itself is never cleared, so the
    /// caller inserts these rather than replacing the whole map.
    repl_lifecycle: HashMap<WsKey, String>,
    default_workspace_slug: Option<String>,
}

/// Project one host's `WorkspaceInfo` rows into what `fe.sessions`
/// declares (session-listing brief decision 2) — a free function, no
/// `State` dependency, so the projection is directly unit-testable. Only
/// rows with a non-empty `agent_handle` are declared: a row that never
/// joined names nobody (ADR 0046), and inventing a name for it is the
/// confident-but-wrong failure this feature exists to close. The other
/// three fields are copied through verbatim — no derived state, so the
/// hub can never be MORE wrong than the strip the person on this box
/// sees.
pub(in crate::ui) fn declared_sessions_from(
    workspaces: &[crate::net::transport::WorkspaceInfo],
) -> Vec<sot_protocol::DeclaredSession> {
    workspaces
        .iter()
        .filter(|w| !w.agent_handle.is_empty())
        .map(|w| sot_protocol::DeclaredSession {
            handle: w.agent_handle.clone(),
            state: w.agent_state.clone(),
            summary: w.agent_summary.clone(),
            status_at: w.agent_status_at.clone(),
        })
        .collect()
}

/// Pure core of the ADR 0042 L2a workspace-cache rebuild: given the union
/// (`ordered_hosts` for display order, `lists` for each host's last-known
/// `workspace.list`) and `active_host`, computes every workspace-scoped
/// cache keyed by `(host, slug)` — no `State` dependency, so "two hosts
/// each having a workspace with the same slug don't collide" is directly
/// unit-testable. `default_workspace_slug` resolves only from
/// `active_host`'s own flagged row — "the default workspace of the
/// connection we're on".
///
/// 2026-09-05: an inert-anchor row (`WorkspaceInfo::is_inert_anchor`,
/// shared with `session_host_children`'s tree filter) is excluded from
/// `workspace_slugs`/`workspace_labels` — see the inline comment below for
/// what stays and why. Without this the row was invisible in the Sessions
/// TREE (#202) but still showed up in the bottom session STRIP, which is
/// built from this function, not from the tree.
fn fresh_workspace_caches(
    ordered_hosts: &[HostKey],
    lists: &HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>>,
    active_host: &HostKey,
) -> FreshWorkspaceCaches {
    let mut out = FreshWorkspaceCaches {
        workspace_slugs: Vec::new(),
        workspace_labels: HashMap::new(),
        workspace_project_roots: HashMap::new(),
        workspace_states: HashMap::new(),
        workspace_id_slugs: HashMap::new(),
        repl_lifecycle: HashMap::new(),
        default_workspace_slug: None,
    };
    for host in ordered_hosts {
        let Some(list) = lists.get(host) else {
            continue;
        };
        for w in list {
            let ws_key: WsKey = (host.clone(), w.slug.clone());
            let is_inert = w.is_inert_anchor();
            if !is_inert {
                let label = if w.label.is_empty() {
                    w.slug.clone()
                } else {
                    w.label.clone()
                };
                out.workspace_labels.insert(ws_key.clone(), label);
            }
            let root = w.canonical_root.as_ref().unwrap_or(&w.project_root); // the watcher's spelling, when sent
            out.workspace_project_roots.insert(ws_key.clone(), root.clone());
            out.workspace_states.insert(
                ws_key.clone(),
                (w.agent_state.clone(), w.agent_status_at.clone()),
            );
            out.workspace_id_slugs
                .insert((host.clone(), w.workspace_id.clone()), ws_key.clone());
            if !w.repl_state.is_empty() {
                out.repl_lifecycle
                    .insert(ws_key.clone(), w.repl_state.clone());
            }
            // The inert anchor (session_host_children's filter, above) must
            // be hidden from the bottom STRIP too — not pushed into
            // `workspace_slugs`, the list the strip iterates to render rows
            // and workspace-cycle keybindings walk (`workspace_labels`
            // above is likewise skipped: no label to render). Everything
            // else here is kept: `workspace_project_roots` /
            // `workspace_id_slugs` are harmless (the daemon still lists the
            // row), and `default_workspace_slug` below is the strip's own
            // active-index fallback — it must still resolve to this row
            // when it's the connection's default.
            if !is_inert {
                out.workspace_slugs.push(ws_key.clone());
            }
            if w.is_default && host == active_host {
                out.default_workspace_slug = Some(w.slug.clone());
            }
        }
    }
    out
}

/// Activity tier for the bottom strip's within-host ordering (decided
/// 2026-09-08): red, white, blue, green, purple, gray — left to right.
/// Needs-you first: `blocked` (a question pending on the user) ahead of a
/// BADGED row (a result the session deliberately surfaced for the user and
/// they have not looked at — the ADR 0025 badge floor, FE-local, cleared only
/// after its canonical row's matching cursor, preview and successful presentation),
/// ahead of `done` (a turn the user asked
/// for, finished and unread — ADR 0044). Then the busy tiers, `working`
/// before `waiting` (delegated, owed a result), then everything resting
/// (`idle`, empty, unknown). A badge lifts any row except a red one — red
/// stays first whatever else is true of the row.
fn activity_rank(state: &str, badged: bool) -> u8 {
    match (state, badged) {
        ("blocked", _) => 0,
        (_, true) => 1,
        ("done", _) => 2,
        ("working", _) => 3,
        ("waiting", _) => 4,
        _ => 5,
    }
}

/// Reorder `fresh_workspace_caches`' union so each HOST BLOCK lists its
/// rows most-active-first, LEFT to right in the strip (asked
/// 2026-09-08). Host blocks stay contiguous and in their incoming order —
/// this only permutes rows *within* a block, so `strip_items`' dividers
/// land exactly where they did. Pure: the result is a function of the
/// rows, their `(agent_state, agent_status_at)` pairs, the PREVIOUS strip
/// order and the pinned row alone — never the clock — which is what makes
/// it jitter-free: `rebuild_workspace_caches` only runs on a
/// `workspace.list` arrival, and an arrival that changed no state or stamp
/// reproduces the previous order byte-for-byte.
///
/// Sort key within a block, ascending: `activity_rank` (with `badged` —
/// the rows carrying a pending badge-floor result, FE state rather than
/// registry projection, which is why it is a separate input), then the
/// stamp (newest first — RFC 3339, parsed; an unparseable/empty stamp
/// sorts last), then the row's position in `prev` (so full ties keep their
/// standing order instead of following whatever order the daemon happened
/// to list them in), then daemon order for a brand-new row. `pinned` — the
/// selected row — keeps the index it had within its block in `prev`
/// (clamped if the block shrank), so it never slides under the cursor
/// while the rows around it re-rank; once the user moves off it, it settles
/// into rank order at the next state change. A pinned row with no previous
/// standing (first appearance) just ranks like any other.
fn activity_order(
    slugs: &[WsKey],
    states: &HashMap<WsKey, (String, String)>,
    badged: &std::collections::HashSet<WsKey>,
    prev: &[WsKey],
    pinned: Option<&WsKey>,
) -> Vec<WsKey> {
    let prev_pos = |k: &WsKey| prev.iter().position(|p| p == k).unwrap_or(usize::MAX);
    let sort_key = |k: &WsKey| {
        let (state, at) = states
            .get(k)
            .map(|(s, a)| (s.as_str(), a.as_str()))
            .unwrap_or(("", ""));
        let stamp = chrono::DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MIN);
        (activity_rank(state, badged.contains(k)), std::cmp::Reverse(stamp), prev_pos(k))
    };
    let mut out: Vec<WsKey> = Vec::with_capacity(slugs.len());
    let mut start = 0;
    while start < slugs.len() {
        let host = &slugs[start].0;
        let end = start + slugs[start..].iter().take_while(|k| k.0 == *host).count();
        let block = &slugs[start..end];
        let pin = pinned.filter(|p| block.contains(p)).and_then(|p| {
            prev.iter()
                .filter(|k| k.0 == *host)
                .position(|k| k == p)
                .map(|slot| (p, slot.min(block.len() - 1)))
        });
        let mut ranked: Vec<&WsKey> = block
            .iter()
            .filter(|k| pin.map_or(true, |(p, _)| *k != p))
            .collect();
        // Stable: rows tied on every key keep daemon order.
        ranked.sort_by_cached_key(|k| sort_key(k));
        if let Some((p, slot)) = pin {
            ranked.insert(slot, p);
        }
        out.extend(ranked.into_iter().cloned());
        start = end;
    }
    out
}

impl State {
    /// The strip's selected row as a `(host, slug)` key: the active
    /// workspace, else `default_slug` (the caller says which default — the
    /// fresh one during a rebuild, the cached one otherwise) on the active
    /// host. `None` before any workspace is known.
    fn selected_ws_key(&self, default_slug: Option<&str>) -> Option<WsKey> {
        self.active_workspace_id
            .as_deref()
            .or(default_slug)
            .map(|s| (self.active_host.clone(), s.to_string()))
    }

    /// Reconcile canonical pending-result identities before rebuilding workspace caches from an authoritative host list. A disconnect is not removal, and a kept default row retains its identity.
    pub(in crate::ui) fn reconcile_pending_results(
        &mut self,
        host: &HostKey,
        incoming: &[crate::net::transport::WorkspaceInfo],
    ) {
        let owed: Vec<String> = self
            .pending_nav_workspace_ids(host)
            .into_iter()
            .filter(|id| !incoming.iter().any(|row| &row.workspace_id == id))
            .collect();
        for id in owed {
            self.invalidate_result_row(host, &id);
        }
    }

    /// Re-rank the strip in place after a badge was marked or cleared —
    /// the one activity input that changes without a `workspace.list`
    /// arrival. Same pure function, same pin, same stability: a call that
    /// changed nothing reproduces the current order.
    pub(in crate::ui) fn resort_strip(&mut self) {
        let selected = self.selected_ws_key(self.default_workspace_slug.as_deref());
        self.workspace_slugs = activity_order(
            &self.workspace_slugs,
            &self.workspace_states,
            &self.badged_keys(),
            &self.workspace_slugs,
            selected.as_ref(),
        );
    }

    /// Clear + rebuild every workspace-scoped cache from the union of every
    /// host's last-known `workspace.list` (`workspace_lists`), in `conns`
    /// order (ADR 0042 L2a). Called whenever ANY host's slice of the union
    /// changes, not just the active host's. The union→caches computation
    /// itself is `fresh_workspace_caches`, a free function with no `State`
    /// dependency (so "two hosts sharing a slug don't collide" is
    /// unit-testable); this method applies the result and layers on the
    /// one thing that genuinely needs history — flash-on-transition
    /// detection against the PRIOR `prev_workspace_states`.
    pub(in crate::ui) fn rebuild_workspace_caches(&mut self) {
        self.refresh_badge_index();
        let fresh = fresh_workspace_caches(
            &self.ordered_hosts(),
            &self.workspace_lists,
            &self.active_host,
        );
        for (key, (state, _)) in &fresh.workspace_states {
            match self.prev_workspace_states.get(key) {
                Some(prev) if prev != state => {
                    self.flash_starts
                        .insert(key.clone(), std::time::Instant::now());
                }
                _ => {}
            }
        }
        // Union-wide, not `.clear()` + reinsert: a host absent from THIS
        // rebuild (never seen) simply contributes no keys — matches the
        // pre-L2a "not cleared" contract prev_workspace_states has always
        // had (a slug missing from this cycle keeps its last-known prior
        // state rather than losing first-appearance detection on return).
        for (key, (state, _)) in &fresh.workspace_states {
            self.prev_workspace_states
                .insert(key.clone(), state.clone());
        }
        // Strip order = activity order within each host block (decided
        // 2026-09-08); the selected row is pinned to its previous slot.
        // The fallback default slug comes from `fresh`, not `self` — the
        // old one may name a row this rebuild just dropped.
        let selected = self.selected_ws_key(fresh.default_workspace_slug.as_deref());
        self.workspace_slugs = activity_order(
            &fresh.workspace_slugs,
            &fresh.workspace_states,
            &self.badged_keys(),
            &self.workspace_slugs,
            selected.as_ref(),
        );
        self.workspace_labels = fresh.workspace_labels;
        self.workspace_project_roots = fresh.workspace_project_roots;
        self.workspace_states = fresh.workspace_states;
        self.workspace_id_slugs = fresh.workspace_id_slugs;
        // repl_lifecycle is NEVER cleared (old-daemon empty repl_state must
        // not regress a frame-driven entry) — insert, don't replace.
        for (key, state) in fresh.repl_lifecycle {
            self.repl_lifecycle.insert(key, state);
        }
        self.default_workspace_slug = fresh.default_workspace_slug;
        self.migrate_default_slug_keys();
        self.rebuild_connection_status();
    }

    /// Rebuild the chrome status line to reflect the currently active
    /// workspace. Format: `connected · <host>:<workspace_label> · rev N`.
    /// Falls back to the slug if `workspace_labels` hasn't been populated
    /// yet, and to the daemon's project_root basename for the default
    /// workspace. No-op until the hello response has landed (no host).
    pub(in crate::ui) fn rebuild_connection_status(&mut self) {
        // Don't clobber a fresh notify toast: hold it on the status line until
        // its sticky window elapses (a workspace switch would otherwise rebuild
        // over it immediately). Once elapsed, clear the flag and rebuild.
        if let Some(until) = self.notify_sticky_until {
            if std::time::Instant::now() < until {
                return;
            }
            self.notify_sticky_until = None;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        let label = self
            .active_workspace_label()
            .unwrap_or_else(|| "default".to_string());
        self.status = format!("connected · {host}:{label} · rev {}", self.last_revision);
    }

    /// Display label for the active workspace: the per-workspace name from
    /// `workspace.list` (falling back to the slug), or the daemon's
    /// project_root basename for the default workspace. This is also the
    /// basename of the Files-mode root directory, so it doubles as the
    /// expected nav root-row label (used to reconcile a stale root after a
    /// snapshot restore). `None` before the hello response has landed.
    pub(in crate::ui) fn active_workspace_label(&self) -> Option<String> {
        match self.active_workspace_id.as_deref() {
            Some(slug) => {
                let key: WsKey = (self.active_host.clone(), slug.to_string());
                Some(
                    self.workspace_labels
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| slug.to_string()),
                )
            }
            None => self.daemon_root_basename.clone(),
        }
    }
}

#[cfg(test)]
#[path = "workspace_list_tests.rs"]
mod tests;
