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
            out.workspace_project_roots
                .insert(ws_key.clone(), w.project_root.clone());
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

/// Activity tier for the bottom strip's within-host ordering (owner ruling
/// 2026-09-08): red, white, blue, green, purple, gray — left to right.
/// Needs-you first: `blocked` (a question pending on the user) ahead of a
/// BADGED row (a result the session deliberately surfaced for the user and
/// they have not looked at — the ADR 0025 badge floor, FE-local, cleared by
/// the very act of switching to it), ahead of `done` (a turn the user asked
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
/// rows most-active-first, LEFT to right in the strip (owner ask
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
        // Strip order = activity order within each host block (owner ruling
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
mod tests {
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
        // Daemon order is the reverse of the ruling; every stamp is newer
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
        // Owner ruling (2026-09-06, symmetry): same anchor rule as the tree,
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
}
