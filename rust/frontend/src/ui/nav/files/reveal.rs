//! A driven reveal: walking the Files tree open to a path the backend named.

use super::*;
/// Ancestor directory relpaths of a workspace-relative file path, deepest
/// first: `"a/b/c.jl"` → `["a/b", "a"]`. Empty for a root-level path (no
/// `/`). Drives the deep-path reveal's level-by-level expansion
/// (`drive_reveal_step` expands the deepest *visible* one each round-trip).
/// Pure so the ordering — which determines we expand from the bottom up — is
/// unit-testable without a full `State`.
fn ancestor_rels(rel: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut acc = rel;
    while let Some(pos) = acc.rfind('/') {
        acc = &acc[..pos];
        out.push(acc);
    }
    out
}

impl State {
    /// Same-workspace driven open (ADR 0025): show `path` (workspace-relative)
    /// in the Files-mode preview AND move the nav cursor onto its row. This is
    /// the single entry point both `fe.command` (`preview`/`reveal`) and the
    /// `nav.preview` relay use for the same-ws case, so the BE never has to
    /// issue a separate cursor move — one command drives both panes.
    ///
    /// The preview body fires immediately for instant feedback. The cursor
    /// lands now if the row is already visible; otherwise `pending_reveal` is
    /// armed and `drive_reveal_step` expands ancestor dirs asynchronously until
    /// the row materializes (the deep-path case the old code left body-only).
    pub(in crate::ui) fn drive_same_ws_open(&mut self, path: &str) {
        // An in-place open takes over the reveal state; a result attempt no longer owns it.
        self.result_reveal = None;
        // Through the store seam (a direct `self.mode =` would leave another
        // mode's rows on screen as "the Files tree").
        self.force_files_mode();
        let node_id = format!("files:{path}");
        // Fire the preview body up front — don't wait on tree expansion.
        let (fit_w, fit_h) = self.preview_fit_px();
        let generation = self.next_preview_gen();
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::PreviewGet {
            node_id: node_id.clone(),
            workspace_id: self.active_workspace_id.clone(),
            page: None,
            fit_w,
            fit_h,
            generation,
        }) {
            tracing::warn!(error = %e, %node_id,
                "drive_same_ws_open: drop preview.get — channel closed");
            return;
        }
        self.preview_node_id_fired = Some(node_id.clone());
        self.preview_anchor_line = None;
        // The active view IS the active workspace's Files tree by
        // construction (installs route by key; force_files_mode swapped by
        // key above) — the old stale-workspace detection has nothing to
        // detect. Cursor reveal: land now if the row is already present;
        // else expand ancestors asynchronously.
        let visible_idx = self.tree.rows.iter().position(|r| r.node.id == node_id);
        if let Some(idx) = visible_idx {
            tracing::info!(%node_id, "reveal: target already visible — cursor landed");
            self.tree.selected = idx;
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            self.driven_preview_hold_cursor = None;
        } else {
            // Hold the per-frame preview-follow off the (stale) cursor row so it
            // doesn't clobber the driven preview while ancestors expand; the
            // hold lifts when the cursor lands on the target.
            self.driven_preview_hold_cursor = self
                .tree
                .rows
                .get(self.tree.selected)
                .map(|r| r.node.id.clone());
            // Fresh reveal intent: drop the per-level bookkeeping a prior
            // target may have left so `drive_reveal_step` and its once-only
            // force-refresh memo start clean (mirrors the resume-reveal reset).
            // Without clearing `reveal_refetched`, a stale `(old_target, anc)`
            // key could trip the "already refreshed → genuinely gone" early-out
            // and strand this reveal.
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            // Split on whether the Files tree is loaded (has rows yet). This
            // split is what makes the reveal robust AND keeps a late
            // `tree.root` reply from ever rebuilding — and thereby
            // collapsing/clobbering — an already-loaded tree (two independent
            // adversarial reviews, 2026-07-15). The old stale-workspace
            // conjunct is gone: the active view can only be the active ws's
            // Files tree now.
            let files_tree_loaded = self
                .tree
                .rows
                .iter()
                .any(|r| r.node.id.starts_with("files:"));
            if files_tree_loaded {
                // Tree loaded but the target row isn't present yet:
                // `drive_reveal_step` walks DOWN from whichever ancestor dir IS
                // present, one `tree.children` per round-trip (and force-
                // refreshes a stale expanded ancestor once, surfacing a brand-
                // new file). If not even the target's top-level dir is present
                // (a brand-new top-level dir created after this listing) the
                // walk gracefully no-ops — preview still shows; the dir surfaces
                // on the next natural refresh. We deliberately do NOT force a
                // `tree.root` here: a late-arriving `set_root` rebuilds and
                // collapses the loaded tree, stranding an intervening reveal and
                // letting the auto-follow clobber the driven preview.
                self.pending_reveal = Some(node_id);
                self.reveal_awaiting = None;
                self.reveal_refetched = None;
                self.drive_reveal_step(None);
            } else {
                // Files tree NOT loaded FOR THE ACTIVE WS: empty, rows belong to
                // another mode, OR the tree is stamped to a different workspace
                // (`!tree_is_active_ws` — the demo-b-tree-while-demo-a
                // desync). Two bugs converge here: (1) the 2026-07-15 case — the
                // preview body fired (path-based, works) but the cursor-reveal
                // had no anchor row to walk from, so it silently no-op'd and the
                // cursor stranded (sotd.log: a same-ws preview of a deep
                // NAS-symlinked results file issued ZERO tree.children); (2) the
                // 2026-07-19 case — the reveal walked a STALE project's rows
                // whose ancestors never match, same zero-tree.children stranding.
                // Both fixed the same way: load `tree.root` for the ACTIVE ws
                // once and arm the one-shot `pending_switch_reveal` the
                // first-visit switch path uses; the TreeRoot handler rebuilds the
                // rows (routed by key to this view), then runs the reveal —
                // auto-resyncing the visible tree too (the same end state as
                // the maintainer's manual collapse-to-root workaround).
                //
                // Safe to `set_root` here: an UNLOADED tree collapses nothing,
                // and a STALE-ws tree SHOULD be collapsed (it's the wrong
                // project, its expansion state is meaningless). While unloaded/
                // stale, concurrent calls are all anchor-less too, so an anchored
                // reveal can't interleave and be overwritten. Gate against a
                // rapid batch (the 3-back-to-back repro): the daemon answers
                // every `tree.root` independently, so fire exactly one and let
                // later calls just update the latest-wins target.
                self.pending_reveal = None;
                let root_inflight = self.pending_switch_reveal.is_some();
                self.pending_switch_reveal = Some(node_id.clone());
                if root_inflight {
                    tracing::info!(%node_id,
                        "reveal: tree.root already in flight — updated switch-reveal target only");
                } else {
                    tracing::info!(%node_id,
                        "reveal: files tree not loaded — loading tree.root and arming switch-reveal");
                    if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeRoot {
                        mode: "files".to_string(),
                        workspace_id: self.active_workspace_id.clone(),
                    }) {
                        tracing::warn!(error = %e,
                            "drive_same_ws_open: drop tree.root — channel closed");
                        self.pending_switch_reveal = None;
                    }
                }
            }
        }
        self.window.request_redraw();
    }

    /// Arm the reveal of a result attempt's file in the entered row's Files tree: land the cursor now when
    /// its rows are present, else wait on the attempt's own root reply.
    pub(in crate::ui) fn drive_result_reveal(
        &mut self,
        attempt: crate::net::transport::ResultAttemptId,
        node_id: String,
        path: String,
        slug: String,
        restored: bool,
    ) {
        self.preview_node_id_fired = Some(node_id.clone());
        self.preview_anchor_line = None;
        self.result_reveal = Some(attempt.clone());
        // The active view is THIS workspace's Files tree by
        // construction (force_files_mode swapped it in by key);
        // the only remaining question is whether it has rows yet
        // (a first visit's slot is empty until tree.root lands).
        let files_tree_usable = self
            .tree
            .rows
            .iter()
            .any(|r| r.node.id.starts_with("files:"));
        // #4: land the nav cursor on the driven file so cursor +
        // preview stay in sync. Two cases, keyed on `restored`:
        if restored && files_tree_usable {
            // Revisit: restore_workspace_ui put the snapshot tree
            // back and sent NO tree.root, so a tree.root-gated reveal would never fire —
            // the original #4 gap, and exactly the maintainer's case (his was
            // a revisit). The rows are present now, so reveal
            // immediately: `drive_reveal_step` lands a visible row or
            // expands a collapsed ancestor, overriding the stale
            // restored cursor.
            // Hold the per-frame preview-follow off the stale cursor
            // row while a deep (async) reveal lands, so
            // `maybe_fire_preview` can't clobber the driven badge
            // preview with the cursor's file (the post-relaunch
            // badge-consume race). Mirrors `drive_same_ws_open`;
            // `drive_reveal_step` clears the hold when it lands.
            if !self.tree.rows.iter().any(|r| r.node.id == node_id) {
                self.driven_preview_hold_cursor = self
                    .tree
                    .rows
                    .get(self.tree.selected)
                    .map(|r| r.node.id.clone());
            }
            self.pending_reveal = Some(node_id.clone());
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            self.drive_reveal_step(None);
        } else {
            // First visit (a tree.root was requested but its rows
            // aren't in yet), a restored-but-FOREIGN tree, or a
            // restored MODULES tree. The rows we want don't exist yet, so arm a
            // one-shot reveal consumed on the incoming reply (see the
            // result-tree handler). The attempt asks for its own root: an
            // ordinary root reply cannot complete a result's reveal.
            self.pending_switch_reveal = Some(node_id.clone());
            if let Err(e) = self.send(crate::net::transport::OutgoingReq::ResultTree {
                attempt: attempt.clone(),
                request: crate::net::transport::ResultTreeRequest::Root,
            }) {
                tracing::warn!(error = %e,
                    "badge consume: drop tree.root — channel closed");
                self.pending_switch_reveal = None;
                self.abandon_result_attempt(&attempt);
            }
        }
        self.status = format!("nav ← agent (pending) · {path}");
        tracing::info!(%node_id, ws = %slug,
            "pending nav.preview driven on workspace switch");
    }

    /// The children request a reveal step sends for `anc_id`: tagged with the result attempt when the
    /// reveal is a result's, so only that attempt's own reply can advance it.
    fn reveal_children_request(&self, anc_id: &str) -> crate::net::transport::OutgoingReq {
        match &self.result_reveal {
            Some(attempt) => crate::net::transport::OutgoingReq::ResultTree {
                attempt: attempt.clone(),
                request: crate::net::transport::ResultTreeRequest::Children {
                    parent_id: anc_id.to_string(),
                },
            },
            None => crate::net::transport::OutgoingReq::TreeChildren {
                parent_id: anc_id.to_string(),
                workspace_id: self.active_workspace_id.clone(),
            },
        }
    }

    /// Advance an in-flight deep-path reveal (`pending_reveal`). No-op when no
    /// reveal is armed, so it's safe to call unconditionally after every
    /// `tree.children` splice. When the target row is now visible it lands the
    /// cursor and clears the reveal; otherwise it expands the deepest visible
    /// ancestor dir (one `tree.children` request) and waits for the reply to
    /// re-enter here. Self-terminating: if the deepest visible ancestor is
    /// already expanded yet the target still isn't present, the path doesn't
    /// resolve and the reveal is dropped (the preview body already showed).
    pub(in crate::ui) fn drive_reveal_step(&mut self, replied_parent: Option<&str>) {
        let Some(target_id) = self.pending_reveal.clone() else {
            return;
        };
        // Target row visible now → land the cursor and finish.
        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
            self.tree.selected = idx;
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            self.driven_preview_hold_cursor = None;
            self.result_cursor_landed(&target_id);
            // Re-anchor the header/preview onto the landed row. The body was
            // already fetched (preview_node_id_fired == target_id), so this
            // doesn't re-fetch — it just keeps header + body in sync.
            self.maybe_fire_preview();
            self.window.request_redraw();
            tracing::info!(%target_id, "reveal: landed cursor on driven-open target");
            return;
        }
        // Scope reply-driven re-entry to the level the walk is waiting on
        // (codex review, round 3): with request-time expansion, an UNRELATED
        // tree.children reply (watcher refresh, another dir's expand) sees
        // the optimistically-expanded ancestor and would double-refresh — or
        // trip the refetched-still-absent abort before the awaited reply
        // arrived. While a wait is armed, only the awaited dir's own reply
        // advances the walk; the awaited level is cleared here exactly when
        // its reply shows up. (Landing on a visible target above is always
        // allowed — any splice may legitimately surface it.)
        let gate = self
            .reveal_awaiting
            .clone()
            .or_else(|| self.reveal_refetched.as_ref().map(|(_, anc)| anc.clone()));
        if let Some(g) = gate {
            if replied_parent != Some(g.as_str()) {
                return;
            }
            if self.reveal_awaiting.as_deref() == Some(g.as_str()) {
                self.reveal_awaiting = None;
            }
        }
        let Some(rel) = target_id.strip_prefix("files:") else {
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            return;
        };
        // Expand the deepest ancestor that's present but not yet expanded
        // (deepest-first ordering from `ancestor_rels`).
        for anc in ancestor_rels(rel) {
            let anc_id = format!("files:{anc}");
            let Some(row) = self.tree.rows.iter().find(|r| r.node.id == anc_id) else {
                continue;
            };
            if row.expanded {
                // Deepest visible ancestor is expanded but the target isn't
                // among its cached children. For a brand-new file (an agent
                // wrote it after this dir was last listed) the cache is simply
                // stale: `list_dir` does a fresh stat, so re-fetching this dir's
                // children ONCE surfaces the file, `apply_children` inserts it,
                // and the re-entrant `drive_reveal_step` lands the cursor — one
                // loopback round-trip, sub-second. Covers directed preview,
                // reveal, AND badge-consume (all funnel through here), so
                // generate→preview and badge→navigate work on fresh files.
                let key = (target_id.clone(), anc_id.clone());
                if self.reveal_refetched.as_ref() == Some(&key) {
                    // Already force-refreshed this dir for this target and it's
                    // STILL absent → genuinely gone. Stop; the body preview stands.
                    tracing::info!(%target_id, %anc_id,
                        "reveal: re-fetched expanded ancestor, target still absent — stopping");
                    self.pending_reveal = None;
                    self.reveal_awaiting = None;
                    self.reveal_refetched = None;
                    return;
                }
                if let Err(e) = self.send(self.reveal_children_request(&anc_id)) {
                    tracing::warn!(error = %e, %anc_id,
                        "reveal: drop tree.children refresh — channel closed");
                    self.pending_reveal = None;
                    self.reveal_awaiting = None;
                    self.reveal_refetched = None;
                    return;
                }
                self.reveal_refetched = Some(key);
                tracing::info!(%target_id, %anc_id,
                    "reveal: force-refresh expanded ancestor for a fresh (not-yet-listed) file");
                return;
            }
            if !row.node.has_children {
                tracing::info!(%target_id, anc = %anc_id,
                    "reveal: ancestor is a leaf (has_children=false) — stopping");
                self.pending_reveal = None;
                self.reveal_awaiting = None;
                return;
            }
            // Already requested this exact level → keep waiting (don't storm).
            if self.reveal_awaiting.as_deref() == Some(anc_id.as_str()) {
                return;
            }
            if let Err(e) = self.send(self.reveal_children_request(&anc_id)) {
                tracing::warn!(error = %e, %anc_id,
                    "reveal: drop tree.children — channel closed");
                self.pending_reveal = None;
                self.reveal_awaiting = None;
                return;
            }
            tracing::info!(%target_id, anc = %anc_id, "reveal: expanding ancestor");
            // Same request-time disclosure flip as try_expand_selected:
            // apply_children drops replies for collapsed parents, so the
            // reveal's intentional ancestor expand must mark the row now.
            if let Some(r) = self.tree.rows.iter_mut().find(|r| r.node.id == anc_id) {
                r.expanded = true;
            }
            self.reveal_awaiting = Some(anc_id);
            return;
        }
        // No ancestor visible at all (root collapsed, or a root-level file the
        // tree hasn't loaded). Nothing to expand toward — drop the reveal; the
        // preview body already showed.
        tracing::info!(%target_id, "reveal: no visible ancestor to expand — stopping");
        self.pending_reveal = None;
        self.reveal_awaiting = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_rels_is_deepest_first() {
        // Deep-path reveal expands the deepest *visible* ancestor each round-
        // trip, so the ordering must be deepest-first to walk down toward the
        // file one level at a time.
        assert_eq!(ancestor_rels("a/b/c.jl"), vec!["a/b", "a"]);
        // Root-level file: no ancestor dirs to expand (already a child of the
        // expanded root) → empty, so drive_reveal_step lands directly.
        assert!(ancestor_rels("README.md").is_empty());
        // Single dir.
        assert_eq!(ancestor_rels("src/edge.jl"), vec!["src"]);
        // Trailing slash (dir target) still yields its parents, deepest-first.
        assert_eq!(ancestor_rels("a/b/"), vec!["a/b", "a"]);
    }
}
