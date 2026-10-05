//! `drain_events`: applies each daemon event queued for the window (`IncomingEvt`), by variant.
//! Routes only: each variant goes to `on_<variant>` in its owner's `replies.rs`;
//! `note_host_connection` runs first.

use super::*;

impl State {
    #[allow(clippy::too_many_lines, reason = "drains the daemon-event queue, one arm per event; predates the 100-line limit")]
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
                crate::net::transport::IncomingEvt::Connected {
                    session_id,
                    revision,
                    // Already recorded into `self.hosts.declared_host` above,
                    // before this match, keyed by `event_host` -- read
                    // back through `host_label` below rather than a
                    // second binding of the same payload field.
                    host: _,
                    project_root,
                    proxy,
                    resolved,
                    backend_version,
                } => self.on_connected(
                    event_host,
                    session_id,
                    revision,
                    project_root,
                    proxy,
                    resolved,
                    backend_version,
                ),
                crate::net::transport::IncomingEvt::Disconnected { reason } => {
                    self.on_disconnected(event_host, reason)
                }
                crate::net::transport::IncomingEvt::HelloRefused { message } => {
                    self.on_hello_refused(event_host, message)
                }
                crate::net::transport::IncomingEvt::TreeRoot {
                    workspace_id,
                    root,
                    children,
                } => self.on_tree_root(event_host, workspace_id, root, children),
                crate::net::transport::IncomingEvt::TreeChildren {
                    workspace_id,
                    parent_id,
                    children,
                } => self.on_tree_children(event_host, workspace_id, parent_id, children),
                crate::net::transport::IncomingEvt::TreeChildrenFailed {
                    workspace_id,
                    parent_id,
                    error,
                } => self.on_tree_children_failed(event_host, workspace_id, parent_id, error),
                crate::net::transport::IncomingEvt::ProjectScan {
                    workspace_id,
                    project_root,
                    package_name,
                    entry_file,
                    modules,
                    generation,
                } => self.on_project_scan(
                    event_host,
                    workspace_id,
                    project_root,
                    package_name,
                    entry_file,
                    modules,
                    generation,
                ),
                crate::net::transport::IncomingEvt::FileParseFailed { workspace_id, path } => {
                    self.on_file_parse_failed(event_host, workspace_id, path)
                }
                crate::net::transport::IncomingEvt::FileParsed {
                    workspace_id,
                    path,
                    ast_hash,
                    definitions,
                } => self.on_file_parsed(event_host, workspace_id, path, ast_hash, definitions),
                crate::net::transport::IncomingEvt::FunctionMethodsReceived {
                    workspace_id,
                    module,
                    name,
                    methods,
                } => self.on_function_methods_received(
                    event_host,
                    workspace_id,
                    module,
                    name,
                    methods,
                ),
                crate::net::transport::IncomingEvt::ConceptRead {
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
                crate::net::transport::IncomingEvt::Preview {
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
                crate::net::transport::IncomingEvt::FigureLoaded { url, mime, bytes } => {
                    self.on_figure_loaded(url, mime, bytes)
                }
                crate::net::transport::IncomingEvt::FigureGetFailed { url } => {
                    self.on_figure_get_failed(url)
                }
                crate::net::transport::IncomingEvt::MathRendered {
                    latex,
                    svg_bytes,
                    ex,
                    display,
                } => self.on_math_rendered(latex, svg_bytes, ex, display),
                crate::net::transport::IncomingEvt::MarkdownTokens {
                    lang,
                    source_hash,
                    spans,
                } => self.on_markdown_tokens(lang, source_hash, spans),
                crate::net::transport::IncomingEvt::ReplEvalDone {
                    eval_id,
                    elapsed_ms,
                    frames,
                } => self.on_repl_eval_done(event_host, eval_id, elapsed_ms, frames),
                crate::net::transport::IncomingEvt::MonitorSubscribed { hosts, .. } => {
                    self.on_monitor_subscribed(hosts)
                }
                crate::net::transport::IncomingEvt::MonitorHistory { hosts } => {
                    self.on_monitor_history(hosts)
                }
                crate::net::transport::IncomingEvt::MonitorTick { hosts } => self.on_monitor_tick(hosts),
                crate::net::transport::IncomingEvt::ReplFrameStreamed {
                    eval_id,
                    workspace_id,
                    frame,
                } => self.on_repl_frame_streamed(event_host, eval_id, workspace_id, frame),
                crate::net::transport::IncomingEvt::ConceptWriteDone { target, result } => {
                    self.on_concept_write_done(target, result)
                }
                crate::net::transport::IncomingEvt::FileRead {
                    node_id,
                    exists,
                    content,
                    version,
                } => self.on_file_read(node_id, exists, content, version),
                crate::net::transport::IncomingEvt::FileWriteDone { node_id, result } => {
                    self.on_file_write_done(node_id, result)
                }
                crate::net::transport::IncomingEvt::FileDeleteDone { node_id, result } => {
                    self.on_file_delete_done(node_id, result)
                }
                crate::net::transport::IncomingEvt::DirCreateDone { node_id, result } => {
                    self.on_dir_create_done(node_id, result)
                }
                crate::net::transport::IncomingEvt::PtyAttachDirect { target } => {
                    self.on_pty_attach_direct(event_host, target)
                }
                crate::net::transport::IncomingEvt::PtyOpenFailed { target, error } => {
                    self.on_pty_open_failed(event_host, target, error)
                }
                crate::net::transport::IncomingEvt::Event { op, payload } => {
                    self.on_event(event_host, op, payload)
                }
                // Sessions-mode pane events (ADR 0013). ADR 0042 L2a
                // codex review deletions: the sibling `tmux.list_sessions`/
                // `tmux.create_session`/`tmux.kill_session` request/reply
                // plumbing had no production
                // sender — ADR 0014 moved Sessions mode onto the daemon's
                // workspace registry (WorkspaceList/Workspaces) instead of
                // scanning tmux, and this dead code still built a
                // pre-L2a, non-host-grouped tree shape that would have
                // been actively wrong had it somehow fired. Panes stay:
                // `tmux.list_panes` (a session's pane list, fired on
                // Sessions-tree row expansion) is live and host-qualified.
                crate::net::transport::IncomingEvt::DirectoryList { path, entries } => {
                    self.on_directory_list(event_host, path, entries)
                }
                crate::net::transport::IncomingEvt::WorkspaceCreated { result } => {
                    self.on_workspace_created(event_host, result)
                }
                crate::net::transport::IncomingEvt::WorkspaceDestroyed { result } => {
                    self.on_workspace_destroyed(event_host, result)
                }
                crate::net::transport::IncomingEvt::PlutoOpened { result } => {
                    self.on_pluto_opened(event_host, result)
                }
                crate::net::transport::IncomingEvt::DocsOpened { result } => {
                    self.on_docs_opened(event_host, result)
                }
                crate::net::transport::IncomingEvt::VideoOpened { result } => {
                    self.on_video_opened(event_host, result)
                }
                crate::net::transport::IncomingEvt::QuartoOpened { result } => {
                    self.on_quarto_opened(result)
                }
                crate::net::transport::IncomingEvt::FileDownloadProgress {
                    dest,
                    written,
                    total,
                    eof,
                } => self.on_file_download_progress(dest, written, total, eof),
                crate::net::transport::IncomingEvt::FileUploadAck {
                    offset: _,
                    done,
                    final_name,
                } => self.on_file_upload_ack(event_host, done, final_name),
                crate::net::transport::IncomingEvt::FileTransferFailed { op, message } => {
                    self.on_file_transfer_failed(event_host, op, message)
                }
                crate::net::transport::IncomingEvt::ImageCropped {
                    node_id,
                    path,
                    x,
                    y,
                    w,
                    h,
                    src_w,
                    src_h,
                } => self.on_image_cropped(event_host, node_id, path, x, y, w, h, src_w, src_h),
                crate::net::transport::IncomingEvt::ImageCropFailed { node_id, message } => {
                    self.on_image_crop_failed(event_host, node_id, message)
                }
                crate::net::transport::IncomingEvt::ScaleSetFailed { node_id, message } => {
                    self.on_scale_set_failed(node_id, message)
                }
                crate::net::transport::IncomingEvt::PreviewGetFailed {
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
                crate::net::transport::IncomingEvt::ReplRunFileDone { eval_id, result } => {
                    self.on_repl_run_file_done(event_host, eval_id, result)
                }
                crate::net::transport::IncomingEvt::Workspaces { workspaces } => {
                    self.on_workspaces(event_host, workspaces)
                }
                // Per-session accounts (owner-simplified brief,
                // 2026-09-15): store into the picker ONLY if it's still
                // open on the host this reply answers — a slow reply
                // after Esc/commit must not resurrect a closed picker or
                // clobber a newer one opened on a different host.
                crate::net::transport::IncomingEvt::AccountsList { accounts } => {
                    self.on_accounts_list(event_host, accounts)
                }
            }
        }
    }

    fn note_host_connection(&mut self, event_host: &HostKey, evt: &crate::net::transport::IncomingEvt) {
        if let crate::net::transport::IncomingEvt::Connected { host: Some(declared), .. } = &evt {
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
            crate::net::transport::IncomingEvt::Connected { .. } => {
                self.hosts.host_connected.insert(event_host.clone(), true);
            }
            crate::net::transport::IncomingEvt::Disconnected { .. } => {
                self.hosts.host_connected.insert(event_host.clone(), false);
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
            crate::net::transport::IncomingEvt::Connected { .. }
                | crate::net::transport::IncomingEvt::Disconnected { .. }
        ) {
            self.rebuild_and_install_sessions_tree();
            if matches!(self.mode, Mode::Hosts) {
                self.populate_hosts_tree();
            }
        }
    }
}
