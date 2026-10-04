//! Switching the window to another session row: the cycle hotkey and the one switch every path goes through.

use super::*;

impl State {
    /// Cycle to the next or previous workspace in `workspace_slugs`
    /// order — the UNION across every connected host (ADR 0042 L2a), so
    /// cycling can cross hosts. `direction = +1` walks forward
    /// (Shift+Right), `-1` walks backward (Shift+Left). Wraps at both
    /// ends. Resolves the *current* position by matching BOTH
    /// `active_host` and the current slug (`active_workspace_id`, falling
    /// back to `default_workspace_slug` for "we're on the default
    /// workspace") — a bare-slug match alone would be ambiguous the
    /// moment two hosts share a slug. No-op until `workspace.list` has
    /// populated the cache, and a no-op when there's only one workspace
    /// registered (nothing to cycle to). Routes through
    /// `switch_to_workspace` so all the snapshot / repaint / BL-retarget
    /// machinery fires the same way it does for Sessions-Enter.
    ///
    /// `person_driven` is the caller's own provenance, carried through
    /// rather than assumed (2026-09-08 review, finding 3): the real
    /// Shift+Left/Right keyboard handler passes `true`; the FE
    /// command-file dispatch (`FeCommand::CycleWs`, someone else driving
    /// the view) and the `--capture-cycle` test/demo simulation both pass
    /// `false`. Previously this was hardcoded `true` below, so ANY caller
    /// — including the command file — could forge the ADR-0044 "a person
    /// stayed on this view" dwell signal.
    pub(in crate::ui) fn cycle_workspace(&mut self, direction: i32, person_driven: bool) {
        if self.workspace_slugs.len() < 2 {
            return;
        }
        let current_slug = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        let n = self.workspace_slugs.len() as i32;
        let idx = current_slug
            .as_deref()
            .and_then(|s| {
                self.workspace_slugs
                    .iter()
                    .position(|(h, slug)| h == &self.active_host && slug == s)
            })
            .map(|p| p as i32)
            .unwrap_or(0);
        let next = ((idx + direction).rem_euclid(n)) as usize;
        let (next_host, next_slug) = self.workspace_slugs[next].clone();
        let session_name = format!("sot-be-{next_slug}");
        // Flick the brand wheels in the direction of travel (forward = CW). The
        // per-frame decay + redraw live in the bottom-strip block; nudge the
        // event loop so the spin animates even if nothing else is dirty.
        self.wheel_vel = (self.wheel_vel + direction as f32 * WHEEL_FLICK_VEL)
            .clamp(-WHEEL_MAX_VEL, WHEEL_MAX_VEL);
        self.dirty = true;
        self.window.request_redraw();
        self.switch_to_workspace(next_host, Some(next_slug), Some(session_name), person_driven);
    }

    /// Single entry point for "switch the chrome's active workspace".
    /// Drives the full snapshot/restore dance from one place so the
    /// Sessions-Enter handler, the workspace-create handler, and the
    /// future cycle-hotkey all behave identically.
    ///
    /// Steps:
    /// 1. Snapshot the *leaving* workspace's UI so a switch-back is
    ///    instant.
    /// 2. Set `active_workspace_id` to the new slug (`None` = default).
    /// 3. Retarget the BL pty to the new workspace's tmux session.
    /// 4. Try to restore from the entering workspace's snapshot —
    ///    if hit, the chrome repaints from cached state and no wire
    ///    request fires.
    /// 5. Otherwise: clear transient view state (so the leaving
    ///    workspace's preview doesn't bleed), fire `tree.root` against
    ///    the new workspace, and refresh `workspace.list` so the
    ///    Sessions row's `kernel_running` badge stays current.
    /// 6. Persist `last_workspace_id` (and the resumed mode/target)
    ///    for the next launch.
    ///
    /// `session_name` is `Some(name)` when the caller already has the
    /// target name (Sessions-Enter, workspace.create reply); `None`
    /// derives it from `paths::session_name(slug)` semantics —
    /// i.e. `sot-be-<slug>`. The default workspace (`slug = None`)
    /// keeps the current BL pane target.
    ///
    /// `host` (ADR 0042 L2a) is the row/event's own connection — set as
    /// `active_host` FIRST, before anything below fires a request, so
    /// every `self.send(...)` in this function and in the
    /// `attach_session_to_bl` it calls already routes to the NEW host.
    /// This is the one choke point: callers don't need `send_to`.
    ///
    /// `read` is `true` only for the two person-driven switches
    /// (Sessions-Enter, Shift+Left/Right cycling) — it rides the
    /// `workspace.activate` signal below and tells the daemon a person
    /// looked at this workspace, clearing a `done` row's blue (ADR 0044).
    /// Every other caller (agent-driven `switch`, cross-workspace
    /// `--urgent` preview, `workspace.create`'s auto-switch, the destroy
    /// bounce) passes `false`.
    pub(in crate::ui) fn switch_to_workspace(
        &mut self,
        host: HostKey,
        slug: Option<String>,
        session_name: Option<String>,
        person_driven: bool,
    ) {
        let old_tree_key = self.retarget_workspace(host, slug, session_name, person_driven);
        let restored = self.restore_entering_workspace();
        let files_root_inflight = self.load_entering_tree(old_tree_key);
        // Refresh the workspace list so kernel_running / new rows stay
        // current — cheap and not user-facing if Sessions mode isn't
        // visible. The reply just updates the cached registry view.
        let _ = self.send(crate::net::transport::OutgoingReq::WorkspaceList);
        // Update the connection status now so the chrome reflects the
        // new workspace immediately. The attach_session_to_bl call above
        // briefly sets a transient "attached BL → …" message; rebuild
        // *after* that so the persistent label wins. The next workspace
        // .list response will refresh it again (kernel_running may flip).
        self.rebuild_connection_status();
        self.persist_resume_state();
        self.consume_pending_nav_badge(restored, files_root_inflight);
        self.window.request_redraw();
    }

    fn retarget_workspace(
        &mut self,
        host: HostKey,
        slug: Option<String>,
        session_name: Option<String>,
        person_driven: bool,
    ) -> TreeKey {
        self.snapshot_current_workspace_ui();
        self.snapshot_current_workspace_repl();
        // The departing tree's key, computed while (mode, workspace) still
        // describe what `self.tree` holds. The swap itself runs after the
        // snapshot-restore below has settled the entering mode.
        let old_tree_key = self.active_tree_key();
        self.active_host = host;
        // Manager review (round 2, finding 14): project the declaration
        // into the status line HERE too, not only in `drain_events`'s own
        // `Connected` handling — switching to a host that is ALREADY
        // connected fires no new `Connected` event, so without this
        // `self.host` kept showing whatever the PREVIOUSLY active host
        // had declared until its own next reconnect.
        self.host = Some(host_label(&self.hosts.declared_host, &self.active_host).to_string());
        // ADR 0042 L2a: preview_fatal is a lazily-rebuilt PROJECTION of
        // protocol_mismatch for whichever host is active (rebuild_fatal_overlay
        // only refills it when it's None) -- an active-host switch must
        // invalidate it, or a stale overlay built for the DEPARTING host
        // could keep showing (or a real mismatch on the ENTERING host could
        // stay hidden behind an empty cached buffer) until something else
        // happens to clear it.
        self.preview_fatal = None;
        self.active_workspace_id = slug.clone();
        // Explicit "this connection's view is now `slug`" signal
        // (`workspace.activate`) — fired UNCONDITIONALLY, before any of the
        // cache-dependent work below (the snapshot-restore may find
        // everything cached and fire no other request at all; a switch back
        // to the default workspace, `slug: None`, fires no `pty.open`
        // either). Both are exactly the cases where the daemon's
        // `preview.changed` fan-out filter used to have nothing to learn the
        // new active workspace from and could sit on the stale one
        // indefinitely (Codex review) — this is the one signal it can
        // always count on. `self.active_host` is already the NEW host (set
        // just above), so this routes correctly even on a cross-host switch.
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::WorkspaceActivate {
            workspace_id: slug.clone(),
            read: false,
        }) {
            tracing::warn!(error = %e, "drop workspace.activate on switch — channel closed");
        }
        // ADR 0044 dwell: a PERSON's switch arms a 10 s read mark for this
        // exact view; any switch (person or not) replaces it, so only a row
        // the user stayed on gets `read: true` (sent from `fire_due_read_mark`).
        self.read_mark = person_driven.then(|| ReadMark {
            host: self.active_host.clone(),
            workspace_id: slug.clone(),
            at: std::time::Instant::now() + READ_DWELL,
        });
        // A workspace change invalidates any one-shot reveal armed for the
        // PREVIOUS workspace. Its `tree.root` reply is dropped by the TreeRoot
        // workspace guard WITHOUT consuming `pending_switch_reveal`, so a stale
        // target would otherwise be picked up by the NEW workspace's root reply
        // and drive the cursor/preview to a same-relative-path file in the
        // wrong project (Codex review 2026-07-15). The first-visit badge-consume
        // below re-arms it for the new workspace when one is pending.
        self.pending_switch_reveal = None;
        // The in-flight deep-reveal bookkeeping is likewise the DEPARTING
        // workspace's: its awaited parent_id names a row in the departing
        // tree, and a same-string parent in the entering tree is a different
        // node. Clearing here is also what lets the TreeChildren park branch
        // skip abort logic entirely — an armed reveal's awaited parent always
        // belongs to the ACTIVE key.
        self.pending_reveal = None;
        self.reveal_awaiting = None;
        self.reveal_refetched = None;
        // The preview-follow hold is the departing reveal's too: it names a
        // node id in the OLD workspace's tree, and the same id exists in
        // most projects (files:README.md) — left armed, it would suppress
        // the ENTERING workspace's cursor-follow preview until the user
        // moved the cursor (blank preview on switch).
        self.driven_preview_hold_cursor = None;
        if let Some(target) = session_name.or_else(|| slug.as_ref().map(|s| format!("sot-be-{s}")))
        {
            // `self.active_host` was just set to `host` above, before
            // anything in this function fired a request — correct BY
            // CONSTRUCTION, not a default (see `attach_session_to_bl`'s
            // own doc).
            self.attach_session_to_bl(self.active_host.clone(), target);
        }
        old_tree_key
    }

    fn restore_entering_workspace(&mut self) -> bool {
        let key = self.active_ws_key();
        // Restore REPL state independently of the UI snapshot — they
        // travel in parallel and either may be missing (e.g. a first
        // visit to a workspace whose UI is already cached has no REPL
        // snapshot yet).
        let _ = self.restore_workspace_repl(&key);
        let restored = self.restore_workspace_ui(&key);
        if !restored {
            // First visit — start from a clean slate.
            self.mode = Mode::Files;
            self.preview_node_id_fired = None;
            self.pinned_preview_node_id = None;
            self.preview_src = None;
            self.preview_src_node_id = None;
            // Same invariant as the snapshot restore: a first-visited
            // workspace must not inherit whatever the departing workspace
            // left in these — a stale current_md_node_id/workspace_id
            // would resolve THIS workspace's first figure fetch against
            // the WRONG project, and a stale figure_failed would collapse
            // figures that are perfectly healthy here.
            self.figure_failed.clear();
            self.current_md_node_id = None;
            self.current_md_workspace_id = None;
            // Same invariant as the snapshot restore: the calibration belongs
            // to the previewed raster, so clearing the preview must clear the
            // scale. Otherwise a first visit inherits the departing
            // workspace's nm/px until the next wire reply overwrites it.
            self.preview_scale = None;
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_concept = None;
            self.concept = None;
            self.concept_target_fired = None;
            self.file_ast_hashes.clear();
            self.file_parse_fired.clear();
            self.edit_state = None;
            self.preview_edit = None;
        }
        restored
    }

    fn load_entering_tree(&mut self, old_tree_key: TreeKey) -> bool {
        // Is a FILES tree.root already on its way? Tracked so the badge-consume
        // never fires a second one (Codex R6 — the corrective reload and
        // the badge path both used to be able to request a root).
        let mut files_root_inflight = false;
        // Swap the nav tree through the store now that the entering mode is
        // settled (snapshot-restored, or Files for a first visit). The
        // departing view parks under its own key; the entering (mode, ws)
        // slot — if one was parked — comes back. A slot can only hold its
        // own key's tree, so the old foreign-tree detect-and-refire dance
        // (Codex R5/R6) has nothing left to detect.
        let new_tree_key = self.active_tree_key();
        self.swap_active_tree(old_tree_key, new_tree_key);
        // First visit to this (mode, workspace) — nothing parked — so fire
        // the mode's loader to fill the empty view.
        if self.tree.rows.is_empty() {
            match self.mode {
                Mode::Files => {
                    tracing::info!("tree.root requested: workspace switch (empty Files slot)");
                    if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeRoot {
                        mode: "files".to_string(),
                        workspace_id: self.active_workspace_id.clone(),
                    }) {
                        tracing::warn!(error = %e, "drop tree.root after workspace switch");
                    } else {
                        files_root_inflight = true;
                    }
                }
                Mode::Modules => {
                    tracing::info!("project.scan requested: workspace switch (empty Modules slot)");
                    let generation = self
                        .next_project_scan_gen(self.active_host.clone(), self.active_workspace_id.clone());
                    if let Err(e) = self.send(crate::net::transport::OutgoingReq::ProjectScan {
                        workspace_id: self.active_workspace_id.clone(),
                        generation,
                    }) {
                        tracing::warn!(error = %e, "drop project.scan after workspace switch");
                    }
                }
                // Global scopes don't depend on the workspace; an empty view
                // here just means they were never loaded this session.
                // ADR 0042 L2a codex review, item A: same fan-out as
                // enter_mode's Sessions arm — the tree spans every host.
                Mode::Sessions => {
                    for (host, _) in &self.conns {
                        let _ = self.send_to(host, crate::net::transport::OutgoingReq::WorkspaceList);
                    }
                }
                Mode::Hosts => {
                    self.populate_hosts_tree();
                    self.select_active_host();
                }
            }
        } else if self.mode == Mode::Files {
            // Revisit: the parked Files tree came back as it was left, and
            // nothing could have updated it meanwhile (watcher refreshes
            // reach only the active view). Re-list its open dirs so files
            // created in this workspace while it was parked appear — same
            // shape as the mode-return refresh in `enter_mode`.
            self.refresh_restored_files_tree();
        }
        files_root_inflight
    }

    fn consume_pending_nav_badge(&mut self, restored: bool, files_root_inflight: bool) {
        // Badge floor (ADR 0025 §1): if we just switched to a workspace that
        // had a pending `nav.preview` result, drive it now and clear the badge.
        // Resolve the switched-to slug the same way `handle_nav_envelope`'s gate
        // does (active id, falling back to the default workspace's slug) so the
        // key matches what `mark_pending_nav` recorded. `self.active_host` is
        // already the switched-to host at this point.
        let switched_slug = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        if let Some(slug) = switched_slug {
            let pending_key: WsKey = (self.active_host.clone(), slug.clone());
            if let Some(path) = self.pending_nav.remove(&pending_key) {
                // The badge just cleared for the row we switched to — it is
                // the pinned row, so the re-rank moves nothing under the cursor.
                self.resort_strip();
                // Through the store seam: a workspace restored in Modules mode
                // parks its Modules tree and brings in its Files slot (empty on
                // a first visit) — never shows modules: rows under Files.
                self.force_files_mode();
                let node_id = format!("files:{path}");
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
                        "pending nav.preview: drop preview.get on switch — channel closed, keeping badge");
                    // The send failed, so nothing will ever land for this
                    // file — put the entry back rather than let the removal
                    // above silently lose the badge on a closed channel.
                    self.pending_nav.insert(pending_key, path);
                } else {
                    self.land_pending_nav_badge(node_id, path, slug, restored, files_root_inflight);
                }
            }
        }
    }

    fn land_pending_nav_badge(
        &mut self,
        node_id: String,
        path: String,
        slug: String,
        restored: bool,
        files_root_inflight: bool,
    ) {
        self.preview_node_id_fired = Some(node_id.clone());
        self.preview_anchor_line = None;
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
            // TreeRoot handler).
            self.pending_switch_reveal = Some(node_id.clone());
            // ...and make sure a reply is actually coming. The
            // Modules-restore case fires nothing (the
            // corrective reload is Files-gated, correctly), so
            // without this the badge would arm a one-shot that never
            // resolves and Files mode would keep showing the Modules
            // tree (Codex R6).
            if !files_root_inflight {
                tracing::info!("tree.root requested: badge consume needs a Files tree");
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeRoot {
                    mode: "files".to_string(),
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e,
                        "badge consume: drop tree.root — channel closed");
                    self.pending_switch_reveal = None;
                }
                // No `files_root_inflight = true` here: this is the
                // last point in the switch that can request a root,
                // so nothing reads it again.
            }
        }
        self.status = format!("nav ← agent (pending) · {path}");
        tracing::info!(%node_id, ws = %slug,
            "pending nav.preview driven on workspace switch");
    }
}
