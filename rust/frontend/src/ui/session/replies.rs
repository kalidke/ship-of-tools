//! Session replies: a host's Connected event (dial record, proxy arming, its workspace-list
//! request, the resume of the active view) and Disconnected; workspace.list, create and destroy;
//! the picker's directory and account lists.

use crate::ui::*;

impl State {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_connected(
        &mut self,
        event_host: HostKey,
        session_id: String,
        revision: u64,
        project_root: Option<String>,
        proxy: bool,
        resolved: crate::transport::ResolvedDial,
        backend_version: String,
    ) {
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
        self.hosts.host_resolved_dial.insert(event_host.clone(), resolved.clone());
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
        self.resume_view_if_active(event_host);
    }

    fn resume_view_if_active(&mut self, event_host: HostKey) {
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

    pub(crate) fn on_disconnected(&mut self, event_host: HostKey, reason: String) {
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
            let shown = host_label(&self.hosts.declared_host, &event_host);
            tracing::info!(host = %shown, %reason, "non-active host disconnected");
        }
    }

    pub(crate) fn on_directory_list(
        &mut self,
        event_host: HostKey,
        path: String,
        entries: Vec<crate::transport::DirEntry>,
    ) {
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

    pub(crate) fn on_workspace_created(
        &mut self,
        event_host: HostKey,
        result: Result<crate::transport::WorkspaceCreatedInfo, String>,
    ) {
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

    pub(crate) fn on_workspace_destroyed(
        &mut self,
        event_host: HostKey,
        result: Result<crate::transport::WorkspaceDestroyedInfo, String>,
    ) {
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

    pub(crate) fn on_workspaces(
        &mut self,
        event_host: HostKey,
        workspaces: Vec<crate::transport::WorkspaceInfo>,
    ) {
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
        if self.hosts.declared_host.get(&event_host) == Some(&frontend_identity().host) {
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

    pub(crate) fn on_accounts_list(
        &mut self,
        event_host: HostKey,
        accounts: Vec<crate::transport::AccountInfo>,
    ) {
        if let Some(p) = self.workspace_picker.as_mut() {
            if p.host == event_host {
                p.accounts = accounts;
                p.account_selected = 0;
            }
        }
    }
}
