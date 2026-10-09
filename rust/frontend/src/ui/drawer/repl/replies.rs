//! REPL replies: repl.eval acks, streamed repl.frame output (lifecycle, started runs, browser
//! frames, done) and repl.run_file results, each landing in the live log or its workspace's
//! snapshot.

use crate::ui::*;

impl State {
    pub(crate) fn on_repl_eval_done(
        &mut self,
        event_host: HostKey,
        eval_id: u64,
        elapsed_ms: u64,
        frames: Vec<sot_protocol::ReplFrame>,
    ) {
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

    pub(crate) fn on_repl_frame_streamed(
        &mut self,
        event_host: HostKey,
        eval_id: u64,
        workspace_id: Option<String>,
        frame: sot_protocol::ReplFrame,
    ) {
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
            return;
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
            self.apply_streamed_frame(event_host, eval_id, workspace_id, frame);
        }
    }

    fn apply_streamed_frame(
        &mut self,
        event_host: HostKey,
        eval_id: u64,
        workspace_id: Option<String>,
        frame: sot_protocol::ReplFrame,
    ) {
        let _ = workspace_id;
        // ADR 0032: a `browser` frame is an action, not log content —
        // the eval served a live interactive artifact (WGLMakie/Bonito
        // figure) at a loopback URL. Hand it straight to the OS
        // browser-open (reusing the pluto/video/docs path) and skip the
        // repl-log append entirely. The URL resolves directly on a
        // local FE and through this window's page proxy (ADR 0035) on a remote one.
        if let ReplFrame::Browser { url, open, fe } = &frame {
            let url = url.clone();
            let origin = crate::browser_open::origin_of(&url);
            // `open: false` serves without opening; `fe` names the one
            // frontend that opens it anyway (`wglshow(fig; open = "<fe>")`),
            // matched exactly as a directed fe.command is.
            if !crate::browser_open::opens_here(*open, fe.as_deref(), &self_comm_handle()) {
                tracing::info!(page = %origin, "wgl: browser frame served, not opened here");
                self.status = "interactive figure served, not opened here".to_string();
                self.window.request_redraw();
                return;
            }
            if let Some(url) = self.ensure_proxy_for_url(&event_host, &url, crate::ui::page_proxy::PageSource::Announced) {
                match crate::browser_open::open_page(&url) {
                    Ok(()) => {
                        self.status = format!("opened interactive figure · {origin}")
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, page = %origin, "wgl: browser open failed");
                        self.status =
                            format!("interactive figure · browser-open failed · {e}");
                    }
                }
            }
            self.window.request_redraw();
            return;
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
                    // Not the frame itself: its text can carry anything the eval printed.
                    tracing::debug!(eval_id, "repl.frame: append");
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

    pub(crate) fn on_repl_run_file_done(
        &mut self,
        event_host: HostKey,
        eval_id: u64,
        result: Result<crate::net::transport::ReplRunFileInfo, String>,
    ) {
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
}
