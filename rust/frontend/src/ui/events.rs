//! `drain_events`: applies each daemon event queued for the window (`IncomingEvt`), by variant.

use super::*;

impl State {
    pub(super) fn drain_events(&mut self) {
        while let Ok((event_host, evt)) = self.evt_rx.try_recv() {
            // ADR 0046 decision 1: `HostKey` is never re-homed —
            // `event_host` (the dial label) stays the key for everything
            // below, unshadowed. The declared host is recorded for
            // display (`host_label`) — manager review, S8: closing a
            // duplicate dial here was rejected (no transport shutdown
            // path exists to actually enforce it); the static same-port
            // skip in `dial::resolve_connections` is what prevents a
            // same-daemon collision from ever dialing twice — AND, since
            // the session-listing brief, for the LOCAL-daemon test the
            // `Workspaces` arm below runs on every own-host reply.
            self.note_host_connection(&event_host, &evt);
            match evt {
                crate::transport::IncomingEvt::Connected {
                    session_id,
                    revision,
                    // Already recorded into `self.declared_host` above,
                    // before this match, keyed by `event_host` -- read
                    // back through `host_label` below rather than a
                    // second binding of the same payload field.
                    host: _,
                    project_root,
                    proxy,
                    resolved,
                    backend_version,
                } => {
                    // ADR 0045 decision 1 (Codex review, lane B5 discharge),
                    // reshaped by C3 as amended §5: record exactly which
                    // transport THIS host's control connection actually
                    // resolved to, so `spawn_pane_attach_term`'s capsule
                    // lane dials the SAME one -- never a second, independent
                    // guess. One unconditional insert: the amendment's own
                    // fix for the bug the old `Connected.remote`/`tcp_peer`
                    // pair let through (`remote` was literally `via_tcp`, so
                    // an ssh control connection -- remote and NOT tcp --
                    // recorded `Local` and disarmed its proxy). There is no
                    // "peer address unavailable" arm to port: a recipe
                    // cannot fail to be observed the way `peer_addr()` could.
                    self.host_resolved_dial.insert(event_host.clone(), resolved.clone());
                    // ADR 0035: arm the proxy for THIS host only when its own
                    // daemon can proxy (capability) AND this FE actually
                    // connected to it remotely (not the local pipe). Keyed
                    // on the transport that CONNECTED, not the CLI shape. A
                    // local (pipe) connection to a host reaches that host's
                    // loopback ports directly and never proxies. Per-host
                    // insert/remove, never a single
                    // FE-wide flag: every host's own Connected evt only ever
                    // touches its own entry, so a local pipe daemon on this
                    // box cannot clobber a DIFFERENT host's proxy arming
                    // (the 2026-09-10 incident), and a non-default host's
                    // Connected arms its own entry too, instead of being
                    // silently ignored.
                    if proxy && !matches!(resolved, ResolvedDial::Local) {
                        self.proxy_capable_hosts.insert(event_host.clone());
                    } else {
                        self.proxy_capable_hosts.remove(&event_host);
                    }
                    // Residual (accepted, codex): these gates gate NEW binds
                    // only; listeners already bound this process persist across
                    // a reconnect. A capability DOWNGRADE across reconnect
                    // (proxy true→false, i.e. a daemon swap) leaves stale
                    // listeners — but they degrade gracefully (the new daemon
                    // rejects their proxy.connect → dead page, same as no
                    // proxy), are bounded (one per port, deduped, no leak), and
                    // a daemon swap triggers an FE relaunch per the standing
                    // order. Listener teardown-on-downgrade is a follow-up.
                    self.set_status_line_fields(event_host.clone(), revision, project_root, backend_version);
                    let _ = session_id;
                    self.clear_protocol_mismatch(event_host.clone());
                    // An in-flight file.upload can't survive a transport reset —
                    // its chunk/ack loop is broken and any daemon-side partial is
                    // orphaned. Clear the stranded state (in-flight file AND any
                    // remaining batch) so `u` isn't blocked by a ghost "upload ·
                    // already in progress" (an oversized-chunk frame that reset
                    // the transport used to strand it forever).
                    //
                    // ADR 0042 L2a codex review, item A: EVERY host's own
                    // Connected requests ITS OWN workspace list, not just
                    // active_host's — transport.rs's hello-time fetch is
                    // tree.root only (no workspace.list), so a non-active
                    // host's Sessions-tree node used to stay unreachable
                    // (no children) until the user manually expanded it.
                    // send_to(&event_host, ...) rather than self.send: this
                    // fires for every connection, active or not.
                    let _ = self.send_to(&event_host, crate::transport::OutgoingReq::WorkspaceList);
                    // ADR 0042 L2a: everything from here to the end of this
                    // arm is "MY connection just came up, resume MY view" —
                    // gated on the active host so a NON-active host's own
                    // (re)connect (its tree node just flipped to connected
                    // above) doesn't re-fire the active workspace's resume
                    // flow redundantly.
                    if event_host == self.active_host {
                        // Carry the resumed active workspace across the
                        // reconnect (right after `hello` succeeds — this
                        // whole `Connected` event fires as soon as it does).
                        // The transport's own hello-time fetch just above
                        // (before this event ever reaches the GPU thread) is
                        // always against the DEFAULT workspace and has no
                        // access to chrome state, so it can't send this
                        // itself — hence firing it here rather than baking
                        // it into `hello`'s own payload (the smaller of the
                        // two fixes: no wire-shape change to `hello`, and it
                        // reuses the exact mechanism an ordinary switch
                        // already uses). Fired even when nothing else in
                        // this arm goes on to re-request anything (e.g.
                        // resumed into Sessions mode) — the daemon must
                        // still learn the resumed workspace so its
                        // `preview.changed` fan-out filter doesn't sit on
                        // the default workspace indefinitely (Codex review).
                        // Re-announcing the resumed view after a reconnect,
                        // not a person switching: leave blue as-is.
                        let _ = self.send_to(
                            &event_host,
                            crate::transport::OutgoingReq::WorkspaceActivate {
                                workspace_id: self.active_workspace_id.clone(),
                                read: false,
                            },
                        );
                        let had_batch = self.upload_batch.take().is_some();
                        if self.upload.take().is_some() || had_batch {
                            self.status =
                                format!("upload interrupted by reconnect — {} to retry", self.bindings.first_label(Action::Upload));
                            self.notify_sticky_until =
                                Some(std::time::Instant::now() + NOTIFY_STICKY);
                        }
                        self.rebuild_connection_status();
                        // B5 resume: the transport's hello-time TreeRoot always
                        // requests "files" against the *default* workspace. If
                        // we restored into a different mode — or restored into
                        // Files but with an `active_workspace_id` set (ADR
                        // 0014) — fire the right request now.
                        match self.mode {
                            // ADR 0042 L2a codex review, item A: no separate
                            // fetch here — the unconditional per-Connected
                            // send_to(&event_host, WorkspaceList) above
                            // already covers this host (and every other),
                            // so a second identical request to the SAME
                            // host would just be a redundant round trip.
                            Mode::Sessions => {}
                            Mode::Modules => {
                                let generation = self.next_project_scan_gen(
                                    self.active_host.clone(),
                                    self.active_workspace_id.clone(),
                                );
                                let _ = self.send(crate::transport::OutgoingReq::ProjectScan {
                                    workspace_id: self.active_workspace_id.clone(),
                                    generation,
                                });
                            }
                            Mode::Files => {
                                if self.active_workspace_id.is_some() {
                                    tracing::info!("tree.root requested: hello/reconnect resume");
                                    let _ = self.send(crate::transport::OutgoingReq::TreeRoot {
                                        mode: "files".to_string(),
                                        workspace_id: self.active_workspace_id.clone(),
                                    });
                                }
                                // Arm the post-rebuild cursor restore for the
                                // incoming root (whether fired above or by the
                                // transport's hello-time default fetch). Captured
                                // NOW, while the pre-reconnect tree is intact.
                                if let Some(sel) = self
                                    .tree
                                    .rows
                                    .get(self.tree.selected)
                                    .map(|r| r.node.id.clone())
                                    .filter(|id| id.starts_with("files:") && id != "files:")
                                {
                                    tracing::info!(selected = %sel,
                                    "resume: arming nav cursor restore for the incoming tree.root");
                                    self.restore_nav_after_resume =
                                        Some((self.active_workspace_id.clone(), sel));
                                }
                            }
                            Mode::Hosts => {
                                // ADR 0015: no backend round-trip — the
                                // hosts tree is built from `conns` (the
                                // `--dial` set resolved at startup) on the
                                // frontend side. Populate once on resume;
                                // subsequent `h` re-entries call
                                // `populate_hosts_tree` directly. A resume
                                // is a first population too, so land on
                                // the active host same as mode entry.
                                self.populate_hosts_tree();
                                self.select_active_host();
                            }
                        }
                        // Reconnect after laptop sleep / SSH-tunnel drop:
                        // the backend's tmux master + workspace state survive,
                        // but the per-connection pty reader/writer pair died
                        // with the old transport. Re-fire PtyOpen so the BL
                        // pane resumes streaming bytes instead of sitting on
                        // its pre-suspend buffer.
                        //
                        // ADR 0042 slice L1b: skipped when the session pane is
                        // a capsule attach — `pane_attach_term` owns its OWN
                        // reconnect episode/backoff on a SEPARATE connection
                        // (the capsule's attach lane, not this daemon JSON
                        // transport), so it needs no help from this daemon
                        // reconnect handler and re-firing would only be a
                        // redundant round trip against a row already
                        // correctly attached. That episode is gated by this
                        // host's link gate (written only by the transport):
                        // while the link is down the client dials nothing,
                        // and once this very Connected has opened the gate
                        // the viewed client resumes within one worker tick,
                        // a parked one when it is next viewed.
                        //
                        // SHOULD-FIX (Codex review, lane B5 discharge):
                        // also skipped when this row already carries a
                        // persistent dial CONFIGURATION error
                        // (`pane_dial_error`) — re-firing would only
                        // reproduce the SAME `attach_direct` refusal on
                        // every reconnect forever; a known-broken host is
                        // not retried automatically.
                        let pane_is_capsule = self.pane_attach_term.is_some();
                        if !pane_is_capsule && self.pane_dial_error.is_none() {
                            // ADR 0042 L2a: only re-fire if the OWNING host
                            // is the one that just reconnected -- this
                            // whole arm is already gated on
                            // `event_host == self.active_host`, so in the
                            // normal case owner == event_host by
                            // construction, but a defensive check costs
                            // nothing and documents the invariant here too.
                            if let Some((owner, target)) = self.bl_pane_target.clone() {
                                if owner == event_host {
                                    let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                                    let _ = self.send_to(
                                        &owner,
                                        crate::transport::OutgoingReq::PtyOpen {
                                            cols,
                                            rows,
                                            target: Some(target),
                                            // #5 guard: a reconnect re-attach (sleep /
                                            // tunnel drop) is NOT a user switch — re-stream
                                            // the existing target, don't yank the foreground.
                                            user_switch: false,
                                        },
                                    );
                                }
                            }
                        }
                        // And re-fire preview for the currently-cursored node
                        // so any file changes that landed while we were
                        // disconnected actually show up. preview_node_id_fired
                        // is the source of truth for "what the preview pane is
                        // showing right now".
                        if let Some(node_id) = self.preview_node_id_fired.clone() {
                            let (fit_w, fit_h) = self.preview_fit_px();
                            let generation = self.next_preview_gen();
                            let _ = self.send(crate::transport::OutgoingReq::PreviewGet {
                                node_id,
                                workspace_id: self.active_workspace_id.clone(),
                                // Hold the page across the reconnect — a
                                // blip shouldn't yank a paginated preview
                                // back to page 1.
                                page: self.preview_page.map(|(p, _)| p),
                                fit_w,
                                fit_h,
                                generation,
                            });
                        }
                    } // if event_host == self.active_host
                }
                crate::transport::IncomingEvt::Disconnected { reason } => {
                    if event_host == self.active_host {
                        self.status = format!("disconnected · {reason}");
                    } else {
                        // Manager review (S9, finding S14): `host_label`,
                        // not the bare dial key, so a log line names a
                        // host the same way the tree/status line does.
                        // (Named `shown`, not `display`: tracing's `%`
                        // shorthand expands to `tracing::field::display`
                        // and a same-named local does not play well with
                        // that macro's hygiene.)
                        let shown = host_label(&self.declared_host, &event_host);
                        tracing::info!(host = %shown, %reason, "non-active host disconnected");
                    }
                }
                crate::transport::IncomingEvt::ProtocolMismatch { message } => {
                    self.on_protocol_mismatch(event_host, message)
                }
                crate::transport::IncomingEvt::TreeRoot {
                    workspace_id,
                    root,
                    children,
                } => {
                    // Route by the REPLY's key. Every tree.root this chrome
                    // fires is a Files root, so the reply keys as
                    // (Files, reply workspace). A reply for a key we're not
                    // currently viewing — a stale in-flight root after a
                    // switch, the connect-time default fetch, a files root
                    // while another mode is up — installs into ITS OWN slot
                    // instead of being dropped (old behavior) or clobbering
                    // the active view (the original 2026-05-29 desync). The
                    // active-only side-effects below (cursor defaults,
                    // reveals, capture one-shots) don't apply to a parked
                    // tree.
                    let reply_key: TreeKey = (
                        Mode::Files,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    if reply_key != self.active_tree_key() {
                        // Park ONLY into an empty slot: a root-only rebuild
                        // is STRICTLY POORER than a parked expanded tree
                        // (repro: reconnect arms a restore for ws A, user
                        // switches to B before A's root lands — replacing
                        // A's expanded slot with a root-only view loses the
                        // expansion AND suppresses the return-visit refetch,
                        // since non-empty slots skip the loader). A richer
                        // parked tree wins; drop the reply like the old
                        // guard did.
                        let slot = self.tree_store.slot_mut(reply_key.clone());
                        if slot.view.rows.is_empty() || slot.from_reply {
                            // Empty, or holding an EARLIER parked reply —
                            // replies are server-ordered, newest wins (the
                            // double-toggle race, codex r5). Only user-
                            // stashed state (from_reply=false) is sacred.
                            tracing::info!(
                                ?reply_key,
                                "tree.root parked into its slot (not the active view)"
                            );
                            slot.view.set_root(root, children);
                            slot.from_reply = true;
                        } else {
                            tracing::info!(
                                ?reply_key,
                                "tree.root dropped — parked slot holds user state"
                            );
                        }
                        continue;
                    }
                    // TRACED (2026-07-10 nav-collapse diagnosis): set_root
                    // rebuilds the whole tree (expansion lost) — every
                    // request origin is traced too, so an unexpected
                    // collapse names its trigger in the log.
                    tracing::info!(
                        rows_before = self.tree.rows.len(),
                        new_children = children.len(),
                        "tree.root applied — nav tree rebuilt (set_root)"
                    );
                    self.tree.set_root(root, children);
                    // Reconnect nav restore (2026-07-11): if this root is the
                    // hello/reconnect rebuild, re-reveal the pre-reconnect
                    // cursor through the reveal machinery — its ancestor path
                    // re-expands level by level and the cursor lands back on
                    // the exact row (preview re-anchors on landing). Consumed
                    // once; discarded when the workspace changed in between
                    // (a switch's fresh tree must not chase the old path).
                    if let Some((armed_ws, sel)) = self.restore_nav_after_resume.take() {
                        if armed_ws == self.active_workspace_id {
                            tracing::info!(target = %sel,
                                "resume: restoring nav cursor after reconnect rebuild");
                            self.driven_preview_hold_cursor = self
                                .tree
                                .rows
                                .get(self.tree.selected)
                                .map(|r| r.node.id.clone());
                            self.pending_reveal = Some(sel);
                            self.reveal_awaiting = None;
                            self.reveal_refetched = None;
                            self.drive_reveal_step(None);
                        } else {
                            tracing::info!(?armed_ws, active = ?self.active_workspace_id,
                                "resume: nav restore discarded — workspace changed before the root arrived");
                        }
                    }
                    // Restore the nav cursor persisted across an ADR-0017
                    // relaunch, best-effort: select the saved node id if it's
                    // present in the freshly loaded tree (one-shot, gated by
                    // the workspace check above so it only lands in the
                    // matching workspace's tree). A deeply-collapsed node that
                    // isn't loaded yet just leaves the default cursor. A
                    // CLI --start-selected below still overrides this.
                    let mut resume_landed = false;
                    if let Some((id, scroll)) = self.pending_resume_nav.take() {
                        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == id) {
                            self.tree.selected = idx;
                            self.tree_scroll = scroll;
                            resume_landed = true;
                        }
                    }
                    // Fresh session (no resume landed): default the first
                    // Files-mode cursor to the project README so the preview
                    // opens onto rendered docs instead of the root row.
                    // The one-shot is consumed on the FIRST Files tree.root
                    // either way, so later refreshes never yank the cursor.
                    // `--start-selected` below still overrides.
                    // `--capture-preview` runs skip the README default: it
                    // moves the cursor, and the cursor-driven readme fetch
                    // then lands after (and clobbers) the captured preview —
                    // the first-row "pretend we already fired" suppression
                    // below only holds while the cursor stays on row 0.
                    let ws_key = self.active_ws_key();
                    let readme_default = matches!(self.mode, Mode::Files)
                        && self.capture_preview.is_none()
                        && self.nav_readme_defaulted.insert(ws_key);
                    if !resume_landed && readme_default {
                        if let Some(idx) = self
                            .tree
                            .rows
                            .iter()
                            .position(|r| r.node.label.eq_ignore_ascii_case("readme.md"))
                        {
                            self.tree.selected = idx;
                        }
                    }
                    // Only consume the start-selected one-shot if this
                    // event matches our startup mode; otherwise the files-
                    // mode `tree.root` that always fires at connect would
                    // eat the selection meant for the modules tree.
                    if matches!(self.mode, Mode::Files) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                        if let Some(rel) = self.capture_preview.take() {
                            let node_id = format!("files:{rel}");
                            tracing::info!(%node_id, "firing --capture-preview");
                            let (fit_w, fit_h) = self.preview_fit_px();
                            let generation = self.next_preview_gen();
                            if let Err(e) = self.send(crate::transport::OutgoingReq::PreviewGet {
                                node_id: node_id.clone(),
                                workspace_id: None,
                                page: None,
                                fit_w,
                                fit_h,
                                generation,
                            }) {
                                tracing::warn!(error = %e, %node_id, "drop --capture-preview request — channel closed");
                            }
                            // Record the REAL fired node id. The cursor-
                            // driven auto-fire race this used to paper over
                            // (by pretending the root row was fired) is now
                            // killed at the source — maybe_fire_preview
                            // stands down while capture_preview_armed. The
                            // honest id matters: the pane title and the
                            // ADR-0021 page transport (PgDn/PgUp/n/p re-fire
                            // the *shown* node) both read it — the root-row
                            // lie made a page turn re-fetch the root preview
                            // and clobber the captured node.
                            self.preview_node_id_fired = Some(node_id);
                        }
                        // #4 fix (cursor-reveal-on-switch): a preview driven via
                        // a workspace switch armed a one-shot reveal before this
                        // workspace's rows existed. They're loaded now — land the
                        // cursor on the driven file. `drive_reveal_step` lands a
                        // top-level row directly and expands ancestors for a
                        // nested one. Runs after the resume/README cursor
                        // defaults above so the explicit switch-reveal wins.
                        if let Some(node_id) = self.pending_switch_reveal.take() {
                            // Hold the per-frame preview-follow off the just-applied
                            // README/default cursor while this deep reveal lands, so
                            // `maybe_fire_preview` can't clobber the driven badge
                            // preview with README (the post-relaunch badge-consume
                            // race — two repros 2026-06-30). Mirrors
                            // `drive_same_ws_open`; cleared on landing.
                            if !self.tree.rows.iter().any(|r| r.node.id == node_id) {
                                self.driven_preview_hold_cursor = self
                                    .tree
                                    .rows
                                    .get(self.tree.selected)
                                    .map(|r| r.node.id.clone());
                            }
                            self.pending_reveal = Some(node_id);
                            self.reveal_awaiting = None;
                            self.reveal_refetched = None;
                            self.drive_reveal_step(None);
                        }
                    }
                }
                crate::transport::IncomingEvt::TreeChildren {
                    workspace_id,
                    parent_id,
                    children,
                } => {
                    // Route by the reply's key, like TreeRoot: a lazy-expand
                    // reply for a (workspace, mode) we're no longer viewing
                    // splices into ITS OWN slot, not the active view.
                    let reply_key: TreeKey = (
                        Mode::Files,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    if reply_key != self.active_tree_key() {
                        tracing::info!(?workspace_id, active = ?self.active_workspace_id,
                            %parent_id, "tree.children parked into its own slot");
                        self.tree_store
                            .slot_mut(reply_key)
                            .view
                            .apply_children(&parent_id, children);
                        // No reveal-abort here: switch_to_workspace clears the
                        // reveal bookkeeping, so an armed reveal's awaited
                        // parent always belongs to the ACTIVE key — a parked
                        // reply can never be the awaited one (a same-string
                        // parent_id from another workspace is a different
                        // node; aborting on it was the cross-key bug).
                        continue;
                    }
                    self.tree.apply_children(&parent_id, children);
                    // Advance an in-flight deep-path reveal: this splice may have
                    // just made the next ancestor (or the target row) visible.
                    // No-op when no reveal is armed.
                    if self.pending_reveal.is_some() {
                        tracing::info!(%parent_id, "reveal: re-entering after children splice");
                    }
                    self.drive_reveal_step(Some(&parent_id));
                }
                crate::transport::IncomingEvt::TreeChildrenFailed {
                    workspace_id,
                    parent_id,
                    error,
                } => {
                    // Key-gate: a failed expand for a PARKED workspace's tree
                    // is not the active view's problem — and its parent_id
                    // could string-match an ACTIVE reveal's awaited parent
                    // (same relative path in another project), which must not
                    // abort that reveal. Trace and move on.
                    let reply_key: TreeKey = (
                        Mode::Files,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    if reply_key != self.active_tree_key() {
                        tracing::info!(?workspace_id, %parent_id, %error,
                            "tree.children failure for a non-active slot — ignored");
                        continue;
                    }
                    // A tree.children request errored (backend error frame or
                    // parse failure). Surface it and abort any reveal waiting
                    // on this parent — previously this was warn-and-drop in
                    // the transport and the reveal starved silently.
                    tracing::info!(%parent_id, %error, "tree.children FAILED");
                    self.status = format!("tree expand failed · {parent_id}: {error}");
                    let refetch_gated = self
                        .reveal_refetched
                        .as_ref()
                        .is_some_and(|(_, anc)| anc == &parent_id);
                    if self.reveal_awaiting.as_deref() == Some(parent_id.as_str()) || refetch_gated
                    {
                        // Covers BOTH wait states (codex round 4): a failed
                        // reply for the awaited level OR for the force-
                        // refreshed ancestor would otherwise leave the reveal
                        // gated forever (only that dir's reply advances the
                        // walk now).
                        tracing::info!(%parent_id, "reveal: aborted — awaited children failed");
                        self.pending_reveal = None;
                        self.reveal_awaiting = None;
                        self.reveal_refetched = None;
                    }
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ProjectScan {
                    workspace_id,
                    project_root,
                    package_name,
                    entry_file,
                    modules,
                    generation,
                } => {
                    // `kernel.request` runs off-loop (switch-latency): two
                    // scans fired close together for the SAME (host,
                    // workspace) can complete in EITHER order now, so —
                    // exactly like `preview.get`'s `reply_is_current` —
                    // drop one that isn't the latest generation issued for
                    // its own key. Per-key (not global) because scans for
                    // DIFFERENT workspaces are independently valid in
                    // flight together; see `next_project_scan_gen`.
                    let latest = self
                        .project_scan_req_gen
                        .get(&(event_host.clone(), workspace_id.clone()))
                        .copied()
                        .unwrap_or(0);
                    if generation != latest {
                        tracing::debug!(?workspace_id, generation, latest, %event_host,
                            "drop stale project.scan reply");
                        continue;
                    }
                    tracing::info!(
                        ?workspace_id,
                        ?project_root,
                        ?package_name,
                        ?entry_file,
                        module_count = modules.len(),
                        type_count = modules.iter().map(|m| m.types.len()).sum::<usize>(),
                        fn_count = modules.iter().map(|m| m.functions.len()).sum::<usize>(),
                        "project.scan reply"
                    );
                    // Route by the reply's key (the set_flat hole, closed): a
                    // Modules scan that isn't for the active (Modules, ws)
                    // lands in its own slot — it can no longer replace
                    // another workspace's tree, or ANY tree while Files mode
                    // is up. `scan_project_root` rides the slot so the
                    // parked tree keeps the root it was scanned against.
                    let rows = scan_to_tree_rows(&modules);
                    let reply_key: TreeKey = (
                        Mode::Modules,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    if reply_key != self.active_tree_key() {
                        tracing::info!(?reply_key, active = ?self.active_tree_key(),
                            "project.scan parked into its own slot (not the active view)");
                        let slot = self.tree_store.slot_mut(reply_key);
                        slot.view.set_flat(rows);
                        slot.scan_project_root = project_root;
                        slot.from_reply = true;
                        continue;
                    }
                    self.scan_project_root = project_root;
                    self.tree.set_flat(rows);
                    // Key match implies Modules mode — the old mode gate on
                    // this consume is subsumed.
                    if let Some(n) = self.pending_initial_selection.take() {
                        self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                    }
                }
                crate::transport::IncomingEvt::ModulesList {
                    workspace_id,
                    modules,
                } => {
                    // Synthesize TreeNodes so Modules-mode reuses the same
                    // TreeView rendering as Files-mode. `path` from Linux's
                    // 4e1c8c0 rides along on `payload.path` so the keyboard
                    // handler can issue `file.parse` for module expansion
                    // without re-querying the kernel. Built-ins (no path)
                    // stay unexpandable.
                    let root = TreeNode {
                        id: "modules:".to_string(),
                        label: "modules".to_string(),
                        kind: "modules".to_string(),
                        has_children: !modules.is_empty(),
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let children = modules
                        .into_iter()
                        .map(|m| {
                            let mut payload = serde_json::Map::new();
                            if let Some(p) = m.path.as_ref() {
                                payload.insert(
                                    "path".to_string(),
                                    serde_json::Value::String(p.clone()),
                                );
                            }
                            TreeNode {
                                id: format!("modules:{}", m.name),
                                label: m.name,
                                kind: "module".to_string(),
                                has_children: m.path.is_some(),
                                badges: Vec::new(),
                                payload,
                            }
                        })
                        .collect();
                    // Route by the reply's key (same shape as ProjectScan —
                    // this is the alternate/legacy Modules loader).
                    let reply_key: TreeKey = (
                        Mode::Modules,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    if reply_key != self.active_tree_key() {
                        // Same empty-slot-only rule as the TreeRoot park: a
                        // root+modules rebuild would destroy parked col-2/3
                        // splices.
                        let slot = self.tree_store.slot_mut(reply_key.clone());
                        if slot.view.rows.is_empty() || slot.from_reply {
                            tracing::info!(?reply_key, "modules.list parked into its slot");
                            slot.view.set_root(root, children);
                            slot.from_reply = true;
                        } else {
                            tracing::info!(
                                ?reply_key,
                                "modules.list dropped — parked slot holds user state"
                            );
                        }
                        continue;
                    }
                    self.tree.set_root(root, children);
                    if let Some(n) = self.pending_initial_selection.take() {
                        self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                    }
                }
                crate::transport::IncomingEvt::FileParseFailed { workspace_id, path } => {
                    // Record the failure; the retry gate in
                    // maybe_fire_concept_read re-arms after a backoff. Do
                    // NOT un-latch `file_parse_fired` here — an instant
                    // un-latch let the redraw loop re-fire every frame
                    // against a fast-failing kernel (the ~4.7k req/s storm).
                    //
                    // Ws-gated like the FileParsed success path (codex r3);
                    // host-qualified too (ADR 0042 L2a) for the same
                    // reasoning -- the counter is keyed by workspace-RELATIVE
                    // path, so a late failure fired for another workspace
                    // (or another HOST's colliding path) would advance THIS
                    // workspace's backoff (or hit its retry cap) for a path
                    // it never parsed.
                    let failed_ws_key: WsKey = (
                        event_host.clone(),
                        self.reply_ws_key(workspace_id.as_deref()),
                    );
                    if failed_ws_key == self.active_ws_key() {
                        let e = self
                            .file_parse_retry
                            .entry(path)
                            .or_insert((std::time::Instant::now(), 0));
                        e.0 = std::time::Instant::now();
                        e.1 += 1;
                    }
                }
                crate::transport::IncomingEvt::FileParsed {
                    workspace_id,
                    path,
                    ast_hash,
                    definitions,
                } => {
                    let reply_ws = self.reply_ws_key(workspace_id.as_deref());
                    // ADR 0042 L2a: host-qualified -- two hosts can each
                    // have a workspace at the same slug, and the drift
                    // check's collision concern below (a shared relative
                    // path across two PROJECTS) applies at least as much
                    // across two HOSTS.
                    let reply_ws_key: WsKey = (event_host.clone(), reply_ws);
                    // Drift-badge bookkeeping is ACTIVE-workspace state (both
                    // maps are per-workspace snapshotted), and the drift
                    // check's `path` is workspace-RELATIVE (`files:` strip) —
                    // so a late reply fired for another workspace could
                    // insert a COLLIDING relative path (both projects have a
                    // `src/lib.jl`) into this workspace's map and fake its
                    // drift verdict. Gate on the reply's workspace. The
                    // skipped insert isn't lost: the owning workspace's
                    // restore drops hash-less fire-latches and re-fires.
                    if reply_ws_key == self.active_ws_key() {
                        self.file_parse_retry.remove(&path);
                        self.file_ast_hashes.insert(path.clone(), ast_hash);
                    }
                    // If a module row's payload.path matches, synthesize
                    // child TreeNodes from the parsed definitions and
                    // splice. Files-mode drift-detection callers ignore
                    // `definitions` (they just want ast_hash); modules-mode
                    // expansion callers consume it here. Same wire shape,
                    // both consumers happy. Routed by the reply's TREE key:
                    // the splice lands in the active view only when
                    // (Modules, reply host+ws) is what's on screen; otherwise
                    // in that key's parked slot — module `path`s are
                    // absolute, but two workspaces CAN define the same
                    // module file (a shared package checked out twice, or
                    // now two hosts running the same project), and a
                    // host-blind lookup would cross-splice them.
                    let reply_key: TreeKey = (Mode::Modules, TreeScope::Workspace(reply_ws_key));
                    let splice_active = reply_key == self.active_tree_key();
                    let module_id = {
                        let view = if splice_active {
                            &self.tree
                        } else {
                            &self.tree_store.slot_mut(reply_key.clone()).view
                        };
                        view.rows
                            .iter()
                            .find(|r| {
                                r.node.kind == "module"
                                    && r.node.payload.get("path").and_then(|v| v.as_str())
                                        == Some(path.as_str())
                            })
                            .map(|r| r.node.id.clone())
                    };
                    if let Some(parent_id) = module_id {
                        // Strip the `modules:` prefix to recover the module
                        // name for col-3's `function.methods` call later.
                        // The module's TreeNode lives at parent_id, so this
                        // is the same string the kernel knows it by.
                        let module_name = parent_id
                            .strip_prefix("modules:")
                            .unwrap_or(&parent_id)
                            .to_string();
                        let kids: Vec<TreeNode> = definitions
                            .into_iter()
                            .map(|d| {
                                // Function rows get has_children=true so
                                // Enter/Right fires `function.methods` for
                                // them. Module name rides on payload so the
                                // chrome doesn't have to re-parse the id.
                                // Non-function defs (struct, abstract, …)
                                // stay leaves for now.
                                let is_function = d.kind == "function";
                                let mut payload = serde_json::Map::new();
                                if is_function {
                                    payload.insert(
                                        "module".to_string(),
                                        serde_json::Value::String(module_name.clone()),
                                    );
                                    payload.insert(
                                        "name".to_string(),
                                        serde_json::Value::String(d.name.clone()),
                                    );
                                }
                                TreeNode {
                                    id: format!("{parent_id}:{}", d.name),
                                    label: format!("{} ({})", d.name, d.kind),
                                    kind: d.kind,
                                    has_children: is_function,
                                    badges: Vec::new(),
                                    payload,
                                }
                            })
                            .collect();
                        if splice_active {
                            self.tree.apply_children(&parent_id, kids);
                        } else {
                            self.tree_store
                                .slot_mut(reply_key)
                                .view
                                .apply_children(&parent_id, kids);
                        }
                    }
                }
                crate::transport::IncomingEvt::FunctionMethodsReceived {
                    workspace_id,
                    module,
                    name,
                    methods,
                } => {
                    // Find the function row whose id matches `modules:<mod>:<name>`.
                    // The exact id is what we built when modules-col-2 splice
                    // ran, so reconstruct it from the request echo. Routed by
                    // the reply's tree key (same rationale as FileParsed's
                    // splice — a same-named module in two workspaces must not
                    // cross-splice); the existence check runs within the
                    // ROUTED view.
                    let parent_id = format!("modules:{module}:{name}");
                    let reply_key: TreeKey = (
                        Mode::Modules,
                        TreeScope::Workspace((
                            event_host.clone(),
                            self.reply_ws_key(workspace_id.as_deref()),
                        )),
                    );
                    let splice_active = reply_key == self.active_tree_key();
                    let exists = {
                        let view = if splice_active {
                            &self.tree
                        } else {
                            &self.tree_store.slot_mut(reply_key.clone()).view
                        };
                        view.rows.iter().any(|r| r.node.id == parent_id)
                    };
                    if !exists {
                        tracing::debug!(
                            %parent_id,
                            "function.methods reply for unknown row — ignoring"
                        );
                        continue;
                    }
                    let kids: Vec<TreeNode> = methods
                        .into_iter()
                        .enumerate()
                        .map(|(i, m)| {
                            // `sig` is the standard `string(m)` repr, which
                            // ends in ` @ <module> <file>:<line>`. Trim that
                            // tail for the row label so the parameter
                            // signature reads cleanly; the location lives
                            // on payload for a future jump-to-line UX.
                            let label = m
                                .sig
                                .split_once(" @ ")
                                .map(|(head, _)| head.to_string())
                                .unwrap_or(m.sig.clone());
                            TreeNode {
                                id: format!("{parent_id}#{i}"),
                                label,
                                kind: "method".to_string(),
                                has_children: false,
                                badges: Vec::new(),
                                payload: Default::default(),
                            }
                        })
                        .collect();
                    if splice_active {
                        self.tree.apply_children(&parent_id, kids);
                    } else {
                        self.tree_store
                            .slot_mut(reply_key)
                            .view
                            .apply_children(&parent_id, kids);
                    }
                }
                crate::transport::IncomingEvt::ConceptRead {
                    target,
                    workspace_id,
                    exists,
                    content,
                    generation,
                } => self.on_concept_read(
                    event_host,
                    target,
                    workspace_id,
                    exists,
                    content,
                    generation,
                ),
                crate::transport::IncomingEvt::Preview {
                    node_id,
                    workspace_id,
                    mime,
                    bytes,
                    extras,
                    generation,
                } => self.on_preview(
                    event_host,
                    node_id,
                    workspace_id,
                    mime,
                    bytes,
                    extras,
                    generation,
                ),
                crate::transport::IncomingEvt::FigureLoaded { url, mime, bytes } => {
                    self.on_figure_loaded(url, mime, bytes)
                }
                crate::transport::IncomingEvt::FigureGetFailed { url } => {
                    self.on_figure_get_failed(url)
                }
                crate::transport::IncomingEvt::MathRendered {
                    latex,
                    svg_bytes,
                    ex,
                    display,
                } => self.on_math_rendered(latex, svg_bytes, ex, display),
                crate::transport::IncomingEvt::MarkdownTokens {
                    lang,
                    source_hash,
                    spans,
                } => self.on_markdown_tokens(lang, source_hash, spans),
                crate::transport::IncomingEvt::ReplEvalDone {
                    eval_id,
                    elapsed_ms,
                    frames,
                } => {
                    // ADR 0014 reply routing. Look up which workspace
                    // this eval was fired for; if it matches the active
                    // workspace, mutate the live `repl_log`; otherwise
                    // splice the result into the originating workspace's
                    // snapshot so the user sees the completed entry when
                    // they swap back. An eval with no recorded owner
                    // falls through to the live log (legacy / restart-
                    // gap behavior).
                    // ADR 0009 phase-2: empty-frames + 0-elapsed is an early
                    // *acceptance* ack (the eval was queued, not yet run). The
                    // streamed `Done` frame owns completion — it finalizes the
                    // entry and drops the routing key. So peek here instead of
                    // removing: removing now would orphan the key before the
                    // frames arrive, dropping a swapped-away eval's frames. Only
                    // a legacy synchronous-collect ack (real frames/elapsed)
                    // finalizes + removes inline.
                    let acceptance = frames.is_empty() && elapsed_ms == 0;
                    // ADR 0042 L2a: the owner key is now (host, eval_id) --
                    // each host's daemon assigns eval ids independently, so
                    // a bare eval_id alone can't disambiguate whose "1" this
                    // reply is for. event_host is exactly that host: this
                    // reply arrived over that connection, so no other host's
                    // eval_id could have produced it.
                    let owner_id = (event_host.clone(), eval_id);
                    let owner = if acceptance {
                        self.eval_id_workspace.get(&owner_id).cloned()
                    } else {
                        self.eval_id_workspace.remove(&owner_id)
                    };
                    let active_key = self.active_ws_key();
                    match owner.as_ref() {
                        Some(key) if key != &active_key => {
                            if let Some(snap) = self.workspace_repl_snapshots.get_mut(key) {
                                if let Some(entry) =
                                    snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                {
                                    if !acceptance {
                                        if !frames.is_empty() {
                                            entry.frames = frames;
                                        }
                                        entry.elapsed_ms = elapsed_ms;
                                        entry.in_flight = false;
                                    }
                                } else {
                                    tracing::debug!(
                                        eval_id,
                                        ?key,
                                        "repl.eval reply for unknown id in snapshot — ignoring"
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    eval_id,
                                    ?key,
                                    "repl.eval reply for workspace with no snapshot — ignoring"
                                );
                            }
                        }
                        _ => {
                            if let Some(entry) =
                                self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                            {
                                if !acceptance {
                                    if !frames.is_empty() {
                                        entry.frames = frames;
                                    }
                                    entry.elapsed_ms = elapsed_ms;
                                    entry.in_flight = false;
                                }
                            } else {
                                tracing::debug!(
                                    eval_id,
                                    "repl.eval reply for unknown id — ignoring"
                                );
                            }
                        }
                    }
                }
                crate::transport::IncomingEvt::MonitorSubscribed { hosts, .. } => {
                    self.on_monitor_subscribed(hosts)
                }
                crate::transport::IncomingEvt::MonitorHistory { hosts } => {
                    self.on_monitor_history(hosts)
                }
                crate::transport::IncomingEvt::MonitorTick { hosts } => self.on_monitor_tick(hosts),
                crate::transport::IncomingEvt::ReplFrameStreamed {
                    eval_id,
                    workspace_id,
                    frame,
                } => {
                    // ADR 0009 phase-2 live streaming: append each frame to the
                    // in-flight `repl_log` entry as it arrives (vs the old
                    // synchronous-collect on ReplEvalDone). Routing mirrors
                    // ReplEvalDone — the entry may be in the active log or, if
                    // its workspace was swapped away, that workspace's snapshot.
                    // We key on the recorded eval_id->workspace map (kept until
                    // the terminal ack drops it); `workspace_id` is a hint.
                    // `Done` finalizes (in_flight=false + elapsed); others append.
                    // A `lifecycle` control frame is workspace-level state,
                    // not eval output (its eval_id is 0): the supervisor
                    // announces spawn ("starting" — precompiling, NOT dead),
                    // first-line ("ready"), and death ("dead"). Route it by
                    // the workspace hint (canonical id → slug translation)
                    // and never near the eval-entry lookup below.
                    if let ReplFrame::Lifecycle { state } = &frame {
                        let key = self.lifecycle_store_key(&event_host, workspace_id.as_deref());
                        tracing::info!(host = %key.0, slug = %key.1, %state, "repl.frame: lifecycle");
                        self.repl_lifecycle.insert(key, state.clone());
                        // The Sessions rows bake `repl_state` from the last
                        // workspace.list reply — refresh it so the row's
                        // badge/glance track the transition, not just the
                        // drawer. Rare (2-3 frames per REPL boot) and the
                        // list rebuild already routes/parks correctly by mode.
                        // Targets the frame's OWN host (ADR 0042 L2a) — the
                        // frame may not have come from `active_host`.
                        let _ = self.send_to(&event_host, OutgoingReq::WorkspaceList);
                        self.window.request_redraw();
                        continue;
                    }
                    // Phase 2 (ADR 0033): a `Started` control frame pre-registers
                    // a drawer entry for a run this FE did NOT originate (a
                    // session's repl.execute), so the run's output frames + the
                    // terminal `done` route to it like any local run.
                    if let ReplFrame::Started {
                        origin, display, ..
                    } = &frame
                    {
                        let owner_id = (event_host.clone(), eval_id);
                        if !self.eval_id_workspace.contains_key(&owner_id) {
                            // Normalize the wire hint through the SAME collapse
                            // current_workspace_key uses: a run in the default
                            // workspace can arrive addressed by its SLUG, and a
                            // raw comparison against "<default>" would route the
                            // entry (and every subsequent frame) to a snapshot
                            // key that no longer exists. Host-qualified (ADR
                            // 0042 L2a): this frame's own event_host, since a
                            // session-originated run can arrive for a
                            // NON-active host.
                            let key: WsKey = (
                                event_host.clone(),
                                self.reply_ws_key(workspace_id.as_deref()),
                            );
                            let label = format!("{origin} ▸ {display}");
                            let new_entry = ReplEntry {
                                eval_id,
                                code: String::new(),
                                frames: Vec::new(),
                                elapsed_ms: 0,
                                in_flight: true,
                                pkg_mode: false,
                                origin: Some(label),
                            };
                            let active_key = self.active_ws_key();
                            if key == active_key {
                                if self.repl_log.len() >= 256 {
                                    self.repl_log.remove(0);
                                }
                                self.repl_log.push(new_entry);
                                self.eval_id_workspace.insert(owner_id, key);
                            } else if let Some(snap) = self.workspace_repl_snapshots.get_mut(&key) {
                                if snap.repl_log.len() >= 256 {
                                    snap.repl_log.remove(0);
                                }
                                snap.repl_log.push(new_entry);
                                self.eval_id_workspace.insert(owner_id, key);
                            } else {
                                tracing::debug!(
                                    eval_id,
                                    host = %key.0,
                                    slug = %key.1,
                                    "repl.frame: started for workspace with no snapshot — skipping"
                                );
                            }
                        }
                        self.window.request_redraw();
                    } else {
                        let _ = workspace_id;
                        // ADR 0032: a `browser` frame is an action, not log content —
                        // the eval served a live interactive artifact (WGLMakie/Bonito
                        // figure) at a loopback URL. Hand it straight to the OS
                        // browser-open (reusing the pluto/video/docs path) and skip the
                        // repl-log append entirely. The URL resolves directly on a
                        // local FE and via the launcher's `-L` tunnel on a remote one.
                        if let ReplFrame::Browser { url, open } = &frame {
                            let url = url.clone();
                            // `open: false` (`wglshow(fig; open=false)`) — the eval
                            // is serving for a TARGETED open: some session will
                            // follow up with `sot-fe open-url <url> --fe <handle>`
                            // for exactly one FE. Every FE must stay hands-off
                            // here (auto-opening on all FEs is the multi-client
                            // layout race the flag exists to avoid); surface the
                            // URL in the status line so a human at any FE can
                            // still open it deliberately.
                            if !open {
                                tracing::info!(%url, "wgl: browser frame served no-open");
                                self.status = format!("interactive figure served · {url}");
                                self.window.request_redraw();
                                continue;
                            }
                            if self.ensure_proxy_for_url(&event_host, &url) {
                                match open_url_in_browser(&url) {
                                    Ok(()) => {
                                        self.status = format!("opened interactive figure · {url}")
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, %url, "wgl: open_url_in_browser failed");
                                        self.status =
                                            format!("interactive figure · browser-open failed · {e}");
                                    }
                                }
                            }
                            self.window.request_redraw();
                            continue;
                        }
                        // Capture the terminal-frame flag before `frame` is moved into
                        // the match below — on Done we run the terminal cleanup the
                        // acceptance ack intentionally deferred to us.
                        let done_elapsed = if let ReplFrame::Done { elapsed_ms, .. } = &frame {
                            Some(*elapsed_ms)
                        } else {
                            None
                        };
                        let owner_id = (event_host.clone(), eval_id);
                        let owner = self.eval_id_workspace.get(&owner_id).cloned();
                        let active_key = self.active_ws_key();
                        let entry: Option<&mut ReplEntry> = match owner.as_ref() {
                            Some(key) if key != &active_key => {
                                self.workspace_repl_snapshots.get_mut(key).and_then(|snap| {
                                    snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                })
                            }
                            _ => self.repl_log.iter_mut().find(|e| e.eval_id == eval_id),
                        };
                        if let Some(entry) = entry {
                            match frame {
                                ReplFrame::Done { elapsed_ms, .. } => {
                                    tracing::debug!(
                                        eval_id,
                                        elapsed_ms,
                                        "repl.frame: done (finalize)"
                                    );
                                    entry.elapsed_ms = elapsed_ms;
                                    entry.in_flight = false;
                                }
                                other => {
                                    // debug, not info — one line per streamed frame
                                    // is too noisy for the default log. Raise to
                                    // RUST_LOG=debug to watch live-append timing.
                                    tracing::debug!(eval_id, frame = ?other, "repl.frame: append");
                                    entry.frames.push(other);
                                }
                            }
                        } else {
                            tracing::warn!(
                                eval_id,
                                "repl.frame dropped: no in-flight entry for eval_id"
                            );
                        }
                        if let Some(done_elapsed) = done_elapsed {
                            // Terminal frame: the acceptance ack deliberately left the
                            // routing key (and, for run_file, the status) for us. Drop
                            // the key and finalize the run_file status with the real
                            // elapsed (the ack's was a 0 placeholder, sent pre-run).
                            self.eval_id_workspace.remove(&owner_id);
                            if let Some((basename, project_dir, fresh)) =
                                self.repl_runfile_status.remove(&owner_id)
                            {
                                self.status = if fresh {
                                    let proj = project_dir.as_deref().unwrap_or("(no project)");
                                    format!(
                                    "ran '{basename}' (fresh — project: {proj}, {done_elapsed}ms)"
                                )
                                } else {
                                    format!("ran '{basename}' (existing repl, {done_elapsed}ms)")
                                };
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                crate::transport::IncomingEvt::ConceptWriteDone { target, result } => {
                    self.on_concept_write_done(target, result)
                }
                crate::transport::IncomingEvt::FileRead {
                    node_id,
                    exists,
                    content,
                    version,
                } => self.on_file_read(node_id, exists, content, version),
                crate::transport::IncomingEvt::FileWriteDone { node_id, result } => {
                    self.on_file_write_done(node_id, result)
                }
                crate::transport::IncomingEvt::FileDeleteDone { node_id, result } => {
                    self.on_file_delete_done(node_id, result)
                }
                crate::transport::IncomingEvt::DirCreateDone { node_id, result } => {
                    self.on_dir_create_done(node_id, result)
                }
                crate::transport::IncomingEvt::PtyAttachDirect { target } => {
                    self.on_pty_attach_direct(event_host, target)
                }
                crate::transport::IncomingEvt::PtyOpenFailed { target, error } => {
                    self.on_pty_open_failed(event_host, target, error)
                }
                crate::transport::IncomingEvt::Event { op, payload } => {
                    self.on_event(event_host, op, payload)
                }
                // Sessions-mode pane events (ADR 0013). ADR 0042 L2a
                // codex review deletions: the sibling `tmux.list_sessions`/
                // `tmux.create_session`/`tmux.kill_session` request/reply
                // plumbing (OutgoingReq::TmuxListSessions/TmuxCreateSession/
                // TmuxKillSession, IncomingEvt::TmuxSessions/
                // TmuxSessionCreated/TmuxSessionKilled) had no production
                // sender — ADR 0014 moved Sessions mode onto the daemon's
                // workspace registry (WorkspaceList/Workspaces) instead of
                // scanning tmux, and this dead code still built a
                // pre-L2a, non-host-grouped tree shape that would have
                // been actively wrong had it somehow fired. Panes stay:
                // `tmux.list_panes` (a session's pane list, fired on
                // Sessions-tree row expansion) is live and host-qualified.
                crate::transport::IncomingEvt::DirectoryList { path, entries } => {
                    // Only consume if it matches the picker we have open
                    // — late replies for a previously-drilled directory
                    // would otherwise overwrite the new entries.
                    if let Some(p) = self.workspace_picker.as_mut() {
                        // ADR 0042 L2a codex review, item K: match the
                        // picker's OWN host too — a directory.list reply
                        // from a different host echoing the same path
                        // (plausible: two hosts share a home-directory
                        // layout) must not populate this picker's entries.
                        if p.current_path == path && p.host == event_host {
                            p.land_listing(entries);
                            self.window.request_redraw();
                        } else {
                            tracing::debug!(%path, current = %p.current_path, %event_host, picker_host = %p.host, "drop stale directory.list reply");
                        }
                    }
                }
                crate::transport::IncomingEvt::WorkspaceCreated { result } => {
                    match result {
                        Ok(info) => {
                            self.workspace_picker = None;
                            self.status = format!(
                                "workspace created · '{}' @ {}",
                                info.label, info.project_root
                            );
                            // The reply arrived over the same connection the
                            // `workspace.create` request targeted (ADR 0042
                            // L2a) — `event_host` IS the new workspace's host.
                            // Auto-switch after create, not a person
                            // arriving at an existing row: leave blue as-is.
                            self.switch_to_workspace(
                                event_host.clone(),
                                Some(info.slug.clone()),
                                Some(info.session_name.clone()),
                                false,
                            );
                            // Land focus in the LLM pane so the freshly
                            // spawned agent is immediately interactive —
                            // without this every create leaves focus in the
                            // nav tree and costs a Ctrl+Arrow hop. Safe to
                            // set after the switch: focus is global, not
                            // part of the restored workspace UI snapshot.
                            // Wide-preview hides the LLM pane (and blocks
                            // focus entry into it) — drop it so the pane
                            // and the focus move are actually visible.
                            // Guard on the preset actually HAVING an Llm
                            // column (codex review): the portrait preset —
                            // and any custom `columns` list — may omit it
                            // entirely, and focusing a slot that is never
                            // laid out would route typed keys into an
                            // invisible pty. Check the BASE preset (we just
                            // cleared wide_preview, whose Llm-less rewrite
                            // is transient).
                            let has_llm = self
                                .settings
                                .resolve_preset(self.monitor_aspect)
                                .columns
                                .contains(&crate::settings::Slot::Llm);
                            if has_llm {
                                self.wide_preview = false;
                                self.set_focus(PaneFocus::Llm);
                            }
                            self.window.request_redraw();
                        }
                        Err(msg) => {
                            self.status = format!("workspace.create failed · {msg}");
                            tracing::warn!(error = %msg, "workspace.create failed");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::WorkspaceDestroyed { result } => {
                    match result {
                        Ok(info) if info.kept.is_some() => {
                            // Default row: the backend ended its capsule
                            // run instead of removing the row — nav/REPL/
                            // tree caches stay untouched. But the BL
                            // pane's own ATTACHMENT is now stale
                            // (`FeAttachClient` marks itself dead but
                            // keeps the rendered screen), and
                            // `attach_session_to_bl`'s unchanged-target
                            // early return would otherwise no-op a future
                            // re-attach forever. Invalidate exactly the
                            // attachment: the live client, buffered
                            // input, `bl_pane_target` (live field AND
                            // this row's own snapshot slot — swap-in
                            // restores FROM the snapshot), and
                            // `pane_feed`.
                            let detail = info.kept.as_deref().unwrap_or("");
                            self.status = format!("{detail} (default row kept)");
                            if self.active_host == event_host
                                && self
                                    .active_workspace_id
                                    .as_deref()
                                    .map(|s| s == info.slug || s == info.workspace_id)
                                    .unwrap_or(false)
                            {
                                self.pane_attach_term = None;
                                self.pane_inputs_discarded = 0;
                                self.bl_pane_target = None;
                                self.pane_feed = PaneFeed::Pending;
                            }
                            let ws_key: WsKey = (
                                event_host.clone(),
                                self.reply_ws_key(Some(info.slug.as_str())),
                            );
                            if let Some(snap) = self.workspace_ui_snapshots.get_mut(&ws_key) {
                                snap.bl_pane_target = None;
                            }
                            // No manual `workspace.list` request here —
                            // the backend's own `run_ended`
                            // `WorkspaceChanged` push (the generic
                            // `WORKSPACE_CHANGED` evt handler above)
                            // already re-lists; a second request here
                            // would just be a duplicate.
                            self.window.request_redraw();
                        }
                        Ok(info) => {
                            // If the active workspace was the one we
                            // just destroyed, bounce to default. The
                            // backend already refused to destroy the
                            // default, so resetting active to None is
                            // always a valid target.
                            // ADR 0042 L2a: also gated on the destroyed
                            // workspace's OWN host matching active_host — a
                            // same-named slug/id destroyed on a DIFFERENT
                            // host must not bounce us off what we're
                            // actually viewing.
                            if self.active_host == event_host
                                && self
                                    .active_workspace_id
                                    .as_deref()
                                    .map(|s| s == info.slug || s == info.workspace_id)
                                    .unwrap_or(false)
                            {
                                // Forced bounce off a destroyed row, not a
                                // person choosing to view it: leave blue as-is.
                                self.switch_to_workspace(event_host.clone(), None, None, false);
                            }
                            // Clean up per-workspace snapshot maps so a
                            // recreated workspace with the same slug
                            // doesn't inherit stale UI/REPL state.
                            // Purge through the SAME key collapse the maps are
                            // written with — a destroyed default-by-slug must
                            // remove the "<default>" entries, not miss them.
                            let ws_key: WsKey = (
                                event_host.clone(),
                                self.reply_ws_key(Some(info.slug.as_str())),
                            );
                            self.workspace_ui_snapshots.remove(&ws_key);
                            self.workspace_repl_snapshots.remove(&ws_key);
                            // The tree slots died with the snapshot on main;
                            // the store split orphaned them — drop the
                            // destroyed workspace's Files/Modules slots so a
                            // same-slug recreate starts clean (codex r2 #3).
                            self.tree_store.purge_workspace(&ws_key);
                            let destroyed_key: WsKey = (event_host.clone(), info.slug.clone());
                            self.workspace_labels.remove(&destroyed_key);
                            self.workspace_project_roots.remove(&destroyed_key);
                            let tmux_note = if info.tmux_killed {
                                ""
                            } else {
                                " (tmux already gone)"
                            };
                            let toml_note = if info.toml_removed {
                                ""
                            } else {
                                " (toml remove failed)"
                            };
                            self.status = format!(
                                "workspace destroyed · '{}'{}{}",
                                info.label, tmux_note, toml_note
                            );
                            // Refresh the Sessions tree so the row drops —
                            // this host's list specifically (ADR 0042 L2a),
                            // not necessarily whatever's active.
                            if let Err(e) = self
                                .send_to(&event_host, crate::transport::OutgoingReq::WorkspaceList)
                            {
                                tracing::warn!(error = %e, "drop workspace.list after destroy");
                            }
                        }
                        Err(msg) => {
                            self.status = format!("workspace.destroy failed · {msg}");
                            tracing::warn!(error = %msg, "workspace.destroy failed");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::PlutoOpened { result } => {
                    self.on_pluto_opened(event_host, result)
                }
                crate::transport::IncomingEvt::DocsOpened { result } => {
                    self.on_docs_opened(event_host, result)
                }
                crate::transport::IncomingEvt::VideoOpened { result } => {
                    self.on_video_opened(event_host, result)
                }
                crate::transport::IncomingEvt::QuartoOpened { result } => {
                    self.on_quarto_opened(result)
                }
                crate::transport::IncomingEvt::FileDownloadProgress {
                    dest,
                    written,
                    total,
                    eof,
                } => self.on_file_download_progress(dest, written, total, eof),
                crate::transport::IncomingEvt::FileUploadAck {
                    offset: _,
                    done,
                    final_name,
                } => self.on_file_upload_ack(event_host, done, final_name),
                crate::transport::IncomingEvt::FileTransferFailed { op, message } => {
                    self.on_file_transfer_failed(event_host, op, message)
                }
                crate::transport::IncomingEvt::ImageCropped {
                    node_id,
                    path,
                    x,
                    y,
                    w,
                    h,
                    src_w,
                    src_h,
                } => self.on_image_cropped(event_host, node_id, path, x, y, w, h, src_w, src_h),
                crate::transport::IncomingEvt::ImageCropFailed { node_id, message } => {
                    self.on_image_crop_failed(event_host, node_id, message)
                }
                crate::transport::IncomingEvt::ScaleSetFailed { node_id, message } => {
                    self.on_scale_set_failed(node_id, message)
                }
                crate::transport::IncomingEvt::PreviewGetFailed {
                    node_id,
                    workspace_id,
                    generation,
                    message,
                } => self.on_preview_get_failed(
                    event_host,
                    node_id,
                    workspace_id,
                    generation,
                    message,
                ),
                crate::transport::IncomingEvt::ReplRunFileDone { eval_id, result } => {
                    // J5: route frames into the pre-registered `repl_log`
                    // entry so the drawer scrollback shows the run's
                    // output alongside any other eval. Cross-workspace
                    // routing mirrors the `ReplEvalDone` handler above:
                    // if the eval was started in a different workspace,
                    // splice into that workspace's snapshot instead of
                    // the live log.
                    // Peek (don't remove): for a streaming run the acceptance ack
                    // arrives before any frame, so removing the key here would
                    // orphan a swapped-away eval's frames. The Done frame drops
                    // the key. Legacy/Err paths remove inline below.
                    // ADR 0042 L2a: owner keyed by (event_host, eval_id) --
                    // this reply's own host, since a session-originated
                    // repl.run_file run can complete on a NON-active host.
                    let owner_id = (event_host.clone(), eval_id);
                    let owner = self.eval_id_workspace.get(&owner_id).cloned();
                    let active_key = self.active_ws_key();
                    match &result {
                        Ok(info) => {
                            let frames = info.frames.clone();
                            let elapsed = info.elapsed_ms;
                            let basename = info
                                .path
                                .rsplit(['/', '\\'])
                                .next()
                                .unwrap_or(info.path.as_str())
                                .to_string();
                            // ADR 0009 phase-2: an empty-frames, 0-elapsed Ok is an
                            // early *acceptance* ack — the run was queued, not yet
                            // executed (so elapsed can only be 0). The streamed
                            // `Done` frame owns completion: it finalizes the entry,
                            // drops the routing key, and sets the final status with
                            // the real elapsed. Here we only stash the display info
                            // (the ack carries the resolved project_dir; the Done
                            // frame doesn't) and show a transient "running" line. A
                            // legacy synchronous-collect Ok finalizes inline.
                            if frames.is_empty() && elapsed == 0 {
                                self.repl_runfile_status.insert(
                                    owner_id,
                                    (basename.clone(), info.project_dir.clone(), info.fresh),
                                );
                                self.status = if info.fresh {
                                    let proj =
                                        info.project_dir.as_deref().unwrap_or("(no project)");
                                    format!("running '{basename}' (fresh — project: {proj})…")
                                } else {
                                    format!("running '{basename}' (existing repl)…")
                                };
                            } else {
                                self.eval_id_workspace.remove(&owner_id);
                                match owner.as_ref() {
                                    Some(key) if key != &active_key => {
                                        if let Some(snap) =
                                            self.workspace_repl_snapshots.get_mut(key)
                                        {
                                            if let Some(entry) = snap
                                                .repl_log
                                                .iter_mut()
                                                .find(|e| e.eval_id == eval_id)
                                            {
                                                if !frames.is_empty() {
                                                    entry.frames = frames;
                                                }
                                                entry.elapsed_ms = elapsed;
                                                entry.in_flight = false;
                                            }
                                        }
                                    }
                                    _ => {
                                        if let Some(entry) =
                                            self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                        {
                                            if !frames.is_empty() {
                                                entry.frames = frames;
                                            }
                                            entry.elapsed_ms = elapsed;
                                            entry.in_flight = false;
                                        }
                                    }
                                }
                                self.status = if info.fresh {
                                    let proj =
                                        info.project_dir.as_deref().unwrap_or("(no project)");
                                    format!(
                                        "ran '{basename}' (fresh — project: {proj}, {elapsed}ms)"
                                    )
                                } else {
                                    format!("ran '{basename}' (existing repl, {elapsed}ms)")
                                };
                            }
                            self.window.request_redraw();
                        }
                        Err(msg) => {
                            tracing::warn!(error = %msg, "repl.run_file failed");
                            // The run failed to start — terminal, no Done frame
                            // will follow, so drop the routing key here.
                            self.eval_id_workspace.remove(&owner_id);
                            // Mark the pre-registered entry done with an
                            // error frame so the drawer reflects the
                            // failure instead of spinning forever.
                            let err_frame = sot_protocol::ReplFrame::Error {
                                message: msg.clone(),
                                stacktrace: Vec::new(),
                            };
                            match owner.as_ref() {
                                Some(key) if key != &active_key => {
                                    if let Some(snap) = self.workspace_repl_snapshots.get_mut(key) {
                                        if let Some(entry) =
                                            snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                        {
                                            entry.frames.push(err_frame);
                                            entry.in_flight = false;
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(entry) =
                                        self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                    {
                                        entry.frames.push(err_frame);
                                        entry.in_flight = false;
                                    }
                                }
                            }
                            self.status = format!("repl.run_file failed · {msg}");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::Workspaces { workspaces } => {
                    // ADR 0014: Sessions mode reads from the daemon's
                    // workspace registry rather than scanning tmux for the
                    // `sot-be-` prefix. Each row carries the canonical
                    // workspace_id + slug in its payload so the swap
                    // handler doesn't have to parse a session name back
                    // into a slug.
                    //
                    // ADR 0042 L2a: this reply is ONE host's list —
                    // `event_host` names which. Replace only that host's
                    // slice of the union (every other host's last-known
                    // list is untouched — an unreachable host keeps
                    // showing its rows, greyed, rather than vanishing),
                    // then rebuild every workspace-scoped cache from the
                    // whole union in one pass.
                    self.workspace_lists.insert(event_host.clone(), workspaces);
                    self.rebuild_workspace_caches();
                    self.prune_warm_attach(&event_host);
                    // No handle declaration is sent for message routing: the daemon files for
                    // its own comm folder (`hub_link.rs`), and a frontend plays no part in it.
                    //
                    // A DIFFERENT declaration — for SESSION LISTING, not
                    // message routing (session-listing brief decision 2)
                    // — IS sent from here: if this reply is the LOCAL
                    // daemon's own (its declared host, recorded above in
                    // `declared_host`, equals this frontend's own host),
                    // project its rows and, only if the projection
                    // changed since the last send, tell every OTHER
                    // connection so a hub that never sees this box's rows
                    // directly can list them.
                    if self.declared_host.get(&event_host) == Some(&frontend_identity().host) {
                        if let Some(rows) = self.workspace_lists.get(&event_host) {
                            let sessions = declared_sessions_from(rows);
                            if self.last_declared_sessions.as_ref() != Some(&sessions) {
                                self.last_declared_sessions = Some(sessions.clone());
                                for (host, _) in &self.conns {
                                    if host == &event_host {
                                        continue;
                                    }
                                    if let Err(e) = self.send_to(
                                        host,
                                        OutgoingReq::FeSessions(sessions.clone()),
                                    ) {
                                        tracing::warn!(
                                            error = %e,
                                            %host,
                                            "drop fe.sessions — channel closed"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    // --capture-cycle <N>: simulate N Ctrl+PgDn presses
                    // (negative = Ctrl+PgUp) on the first workspace.list
                    // reply. Consumed once so a re-fetch from a later
                    // switch doesn't re-cycle.
                    if self.capture_cycle != 0 {
                        let steps = self.capture_cycle;
                        self.capture_cycle = 0;
                        let dir = if steps > 0 { 1 } else { -1 };
                        for _ in 0..steps.abs() {
                            // Simulated cycling (--capture-cycle), not a person.
                            self.cycle_workspace(dir, false);
                        }
                    }
                    // The rest of this handler rebuilds the Sessions-mode
                    // tree (host-grouped — `build_sessions_tree`). Routed
                    // by key below: when another mode is up the rebuilt
                    // rows PARK in the (Sessions, Global) slot instead of
                    // clobbering the active view (the old skip dropped
                    // them; parking keeps the Sessions tree fresh from
                    // switch_to_workspace's workspace.list refreshes, so
                    // entering Sessions shows current rows instantly).
                    self.rebuild_and_install_sessions_tree();
                }
                // Per-session accounts (owner-simplified brief,
                // 2026-09-15): store into the picker ONLY if it's still
                // open on the host this reply answers — a slow reply
                // after Esc/commit must not resurrect a closed picker or
                // clobber a newer one opened on a different host.
                crate::transport::IncomingEvt::AccountsList { accounts } => {
                    if let Some(p) = self.workspace_picker.as_mut() {
                        if p.host == event_host {
                            p.accounts = accounts;
                            p.account_selected = 0;
                        }
                    }
                }
            }
        }
    }

    fn note_host_connection(&mut self, event_host: &HostKey, evt: &crate::transport::IncomingEvt) {
        if let crate::transport::IncomingEvt::Connected { host: Some(declared), .. } = &evt {
            self.record_declared_host(&event_host, declared.clone());
            // Session-listing brief decision 2: a reconnecting hub's
            // connection is brand new and remembers nothing from
            // before, so re-send our last declaration to it right
            // here rather than waiting for the next own-host
            // `workspace.list` reply — which may not come again for a
            // while, and wouldn't resend anyway if the projection
            // hasn't changed. Never sent to the LOCAL daemon itself
            // (that connection's own workspace.list reply is what
            // computes `last_declared_sessions` in the first place).
            if declared != &frontend_identity().host {
                if let Some(sessions) = self.last_declared_sessions.clone() {
                    if let Err(e) =
                        self.send_to(&event_host, OutgoingReq::FeSessions(sessions))
                    {
                        tracing::warn!(
                            error = %e,
                            host = %event_host,
                            "drop fe.sessions resend on reconnect — channel closed"
                        );
                    }
                }
            }
        }
        // ADR 0042 L2a: every host's transport tags its own sends, so
        // per-host connection status is exactly this — no new wire
        // signal, just watching the two evts that already exist.
        match &evt {
            crate::transport::IncomingEvt::Connected { .. } => {
                self.host_connected.insert(event_host.clone(), true);
            }
            crate::transport::IncomingEvt::Disconnected { .. } => {
                self.host_connected.insert(event_host.clone(), false);
            }
            _ => {}
        }
        // ADR 0042 L2a codex review, item L: live host status in the
        // tree. Without this, a node's `connected`/`unreachable`
        // badge only refreshed on the NEXT unrelated event that
        // happened to rebuild the tree (a workspace.list reply for
        // Sessions, a fresh `h`-press for Hosts) — a Connected node
        // could sit `unreachable` and a Disconnected one could sit
        // `connected` indefinitely otherwise. Sessions rebuilds
        // through the SAME install-or-park seam every other trigger
        // uses (a `workspace.list` reply calls this unconditionally
        // too, regardless of the active mode, so doing the same here
        // is not a new pattern). Hosts writes `self.tree` directly
        // (see `populate_hosts_tree`'s own doc, no parked slot), so
        // it's gated on actually being the active view.
        if matches!(
            &evt,
            crate::transport::IncomingEvt::Connected { .. }
                | crate::transport::IncomingEvt::Disconnected { .. }
        ) {
            self.rebuild_and_install_sessions_tree();
            if matches!(self.mode, Mode::Hosts) {
                self.populate_hosts_tree();
            }
        }
    }
}
