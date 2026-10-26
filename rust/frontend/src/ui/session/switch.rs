//! Switching the window to another session row: the cycle hotkey and the one switch every path goes through.

use super::*;

impl State {
    pub(in crate::ui) fn switch_to_resolved_workspace(
        &mut self,
        target: ResolvedWorkspace,
        person_driven: bool,
    ) {
        let attachment = match target.attachment() {
            Ok(attachment) => attachment,
            Err(reason) => {
                self.refuse_result(&reason);
                return;
            }
        };
        self.switch_to_workspace(
            target.row_key().0.clone(),
            target.slug(),
            attachment,
            person_driven,
        );
    }

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
        let Some((next_host, next_slug)) = self.row_presentation.next_row(
            direction, self.selected.host(), self.selected.workspace_id().as_deref(),
        ) else {
            return;
        };
        let target = match self.row_directory.resolve(&next_host, &next_slug) {
            Ok(target) => target,
            Err(reason) => {
                self.refuse_result(&reason);
                return;
            }
        };
        if let Err(reason) = target.attachment() {
            self.refuse_result(&reason);
            return;
        }
        // Flick the brand wheels in the direction of travel (forward = CW). The
        // per-frame decay + redraw live in the bottom-strip block; nudge the
        // event loop so the spin animates even if nothing else is dirty.
        self.strip_motion.flick(direction);
        self.dirty = true;
        self.window.request_redraw();
        self.switch_to_resolved_workspace(target, person_driven);
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
    /// A listed row is attached by its session_name from workspace.list; neither a slug nor a session name is derived from the other.
    /// A create reply can supply its stored name before the next list arrives.
    /// Without a supplied name, resolve the host's current list before switching.
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
        let (slug, session_name) = match session_name {
            Some(name) if !name.is_empty() => (slug, Some(name)),
            Some(_) => {
                self.refuse_result("listed workspace has no attachment target");
                return;
            }
            None => {
                let target = match self.row_directory.resolve(
                    &host,
                    slug.as_deref().unwrap_or(""),
                ) {
                    Ok(target) => target,
                    Err(reason) => {
                        self.refuse_result(&reason);
                        return;
                    }
                };
                let attachment = match target.attachment() {
                    Ok(attachment) => attachment,
                    Err(reason) => {
                        self.refuse_result(&reason);
                        return;
                    }
                };
                (target.slug(), attachment)
            }
        };
        let old_tree_key = self.retarget_workspace(host, slug, session_name, person_driven);
        let restored = self.restore_entering_workspace();
        self.load_entering_tree(old_tree_key);
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
        self.consume_pending_nav_badge(restored);
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
        self.selected.select(host, slug.clone());
        self.row_presentation.select_host_default(&self.row_directory, self.selected.host());
        // (round 2, finding 14): project the declaration
        // into the status line HERE too, not only in `drain_events`'s own
        // `Connected` handling — switching to a host that is ALREADY
        // connected fires no new `Connected` event, so without this
        // the status line kept showing whatever the PREVIOUSLY active host
        // had declared until its own next reconnect.
        self.active_link.relabel(host_label(&self.hosts.declared_host, self.selected.host()).to_string());
        // ADR 0042 L2a: preview_fatal is a lazily-rebuilt PROJECTION of
        // hello_refused for whichever host is active (rebuild_fatal_overlay
        // only refills it when it's None) -- an active-host switch must
        // invalidate it, or a stale overlay built for the DEPARTING host
        // could keep showing (or a real refusal on the ENTERING host could
        // stay hidden behind an empty cached buffer) until something else
        // happens to clear it.
        self.shaped.clear_fatal();
        // `workspace.activate`, fired UNCONDITIONALLY before the cache-dependent work below (a fully cached restore,
        // or a switch to the default row, sends nothing else): the one signal the daemon's `preview.changed` filter
        // can count on (Codex review). The selected host is already the new one, so a cross-host switch reaches it.
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::WorkspaceActivate {
            workspace_id: slug.clone(),
            read: false,
        }) {
            tracing::warn!(error = %e, "drop workspace.activate on switch — channel closed");
        }
        // ADR 0044 dwell: a PERSON's switch arms a 10 s read mark for this
        // exact view; any switch (person or not) replaces it, so only a row
        // the user stayed on gets `read: true` (sent from `fire_due_read_mark`).
        self.selected.arm_read_mark(person_driven, std::time::Instant::now());
        // A workspace change invalidates any one-shot reveal armed for the
        // PREVIOUS workspace. Its `tree.root` reply is dropped by the TreeRoot
        // workspace guard WITHOUT consuming `pending_switch_reveal`, so a stale
        // target would otherwise be picked up by the NEW workspace's root reply
        // and drive the cursor/preview to a same-relative-path file in the
        // wrong project (Codex review 2026-07-15). The first-visit badge-consume
        // below re-arms it for the new workspace when one is pending.
        self.reveal.cancel_root();
        // The in-flight deep-reveal bookkeeping is likewise the DEPARTING
        // workspace's: its awaited parent_id names a row in the departing
        // tree, and a same-string parent in the entering tree is a different
        // node. Clearing here is also what lets the TreeChildren park branch
        // skip abort logic entirely — an armed reveal's awaited parent always
        // belongs to the ACTIVE key.
        self.reveal.abort();
        // Leaving abandons the result attempt that owned the reveal; coming back starts a new one.
        self.result_ledger.release_reveal();
        // The preview-follow hold is the departing reveal's too: it names a
        // node id in the OLD workspace's tree, and the same id exists in
        // most projects (files:README.md) — left armed, it would suppress
        // the ENTERING workspace's cursor-follow preview until the user
        // moved the cursor (blank preview on switch).
        self.preview_requests.leave_row();
        if let Some(target) = session_name {
            // The selected host was just set to `host` above, before
            // anything in this function fired a request — correct BY
            // CONSTRUCTION, not a default (see `attach_session_to_bl`'s
            // own doc).
            self.attach_session_to_bl(self.selected.host().clone(), target);
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
            self.nav.set_mode(Mode::Files);
            self.preview_requests.forget_fired();
            self.nav_cursor.restore_pin(None);
            // Same invariant as the snapshot restore: a first-visited
            // workspace must not inherit the departing workspace's source,
            // its node, its calibration (a stale nm/px until the next wire
            // reply) or its markdown origin (THIS workspace's first figure
            // fetch resolved against the WRONG project) — nor its
            // figure_failed, which would collapse figures that are
            // perfectly healthy here.
            self.preview_source = Default::default();
            self.figures.clear_failed();
            self.raster.hide();
            self.shaped.clear_svg();
            self.concept.clear();
            self.preview_requests.reset_concept_follow();
            self.drift.clear();
            self.editor.close();
        }
        restored
    }

    fn load_entering_tree(&mut self, old_tree_key: TreeKey) {
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
        if self.nav.tree().rows().is_empty() {
            match self.nav.mode() {
                Mode::Files => {
                    tracing::info!("tree.root requested: workspace switch (empty Files slot)");
                    if let Err(e) = self.send(crate::net::transport::OutgoingReq::TreeRoot {
                        mode: "files".to_string(),
                        workspace_id: self.selected.workspace_id().clone(),
                    }) {
                        tracing::warn!(error = %e, "drop tree.root after workspace switch");
                    }
                }
                Mode::Modules => {
                    tracing::info!("project.scan requested: workspace switch (empty Modules slot)");
                    let generation = self
                        .next_project_scan_gen(self.selected.host().clone(), self.selected.workspace_id().clone());
                    if let Err(e) = self.send(crate::net::transport::OutgoingReq::ProjectScan {
                        workspace_id: self.selected.workspace_id().clone(),
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
        } else if self.nav.mode() == Mode::Files {
            // Revisit: the parked Files tree came back as it was left, and
            // nothing could have updated it meanwhile (watcher refreshes
            // reach only the active view). Re-list its open dirs so files
            // created in this workspace while it was parked appear — same
            // shape as the mode-return refresh in `enter_mode`.
            self.refresh_restored_files_tree();
        }
    }

    fn consume_pending_nav_badge(&mut self, restored: bool) {
        // Badge floor (ADR 0025 §1): if we just switched to a workspace that
        // has an owed `nav.preview` result, start an attempt at showing it.
        // The owed entry stays until the cursor, the installed preview and a
        // successful presentation of that file have all arrived.
        let Some(target) = self.active_result_workspace() else {
            return;
        };
        // Starting a matching result attempt keeps the badge until presentation acknowledges it.
        let Some(AttemptStart {
            id: attempt,
            node_id,
            preview_gen: generation,
        }) = self.begin_result_attempt(&target) else {
            return;
        };
        // Through the store seam: a restored Modules view parks and its Files slot comes in.
        self.force_files_mode();
        let (fit_w, fit_h) = self.preview_fit_px();
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::PreviewGet {
            node_id: node_id.clone(),
            workspace_id: self.selected.workspace_id().clone(),
            page: None,
            fit_w,
            fit_h,
            generation,
        }) {
            tracing::warn!(error = %e, %node_id,
                "pending nav.preview: drop preview.get on switch — channel closed, keeping badge");
            self.abandon_result_attempt(&attempt);
            return;
        }
        let path = node_id
            .strip_prefix("files:")
            .unwrap_or(&node_id)
            .to_string();
        self.drive_result_reveal(attempt, node_id, path, target.row_key().1.clone(), restored);
    }
}
