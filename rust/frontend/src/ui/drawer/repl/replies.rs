//! REPL transitions route by event host and eval id. State applies the returned actions.

use super::{BrowserAction, ReplEffects, ReplModel};
use crate::ui::*;

const LINK_BACK: &str = "the connection to the daemon is back, and this eval is still running";

fn end_entry(entry: &mut ReplEntry, elapsed_ms: u64, frames: Vec<ReplFrame>) {
    entry.frames.extend(frames);
    entry.elapsed_ms = elapsed_ms;
    entry.in_flight = false;
}

impl ReplModel {
    /// A recorded but missing parked destination drops output. Only an unknown owner uses the legacy live fallback.
    fn routed_entry(
        &mut self,
        owner: Option<&WsKey>,
        active: &WsKey,
        eval_id: u64,
    ) -> Option<&mut ReplEntry> {
        let log = match owner {
            Some(key) if key != active => &mut self.parked.get_mut(key)?.log,
            _ => &mut self.live.log,
        };
        log.iter_mut().find(|e| e.eval_id == eval_id)
    }

    pub(in crate::ui) fn apply_eval_completion(
        &mut self,
        host: HostKey,
        eval_id: u64,
        active: &WsKey,
        elapsed_ms: u64,
        frames: Vec<ReplFrame>,
    ) {
        let acceptance = frames.is_empty() && elapsed_ms == 0;
        let owner_id = (host, eval_id);
        let owner = if acceptance {
            self.routes.get(&owner_id).cloned()
        } else {
            self.routes.remove(&owner_id)
        };
        if let Some(entry) = self.routed_entry(owner.as_ref(), active, eval_id) {
            if !acceptance {
                end_entry(entry, elapsed_ms, frames);
            }
        } else {
            match owner.as_ref() {
                Some(key) if key != active => {
                    if self.parked.contains_key(key) {
                        tracing::debug!(
                            eval_id,
                            ?key,
                            "repl.eval reply for unknown id in snapshot — ignoring"
                        );
                    } else {
                        tracing::debug!(
                            eval_id,
                            ?key,
                            "repl.eval reply for workspace with no snapshot — ignoring"
                        );
                    }
                }
                _ => tracing::debug!(eval_id, "repl.eval reply for unknown id — ignoring"),
            }
        }
    }

    pub(in crate::ui) fn reopen_runs(
        &mut self,
        host: &HostKey,
        active: &WsKey,
        ids: Vec<u64>,
    ) -> ReplEffects {
        if ids.is_empty() {
            return ReplEffects::default();
        }
        for eval_id in ids {
            let closed = |e: &ReplEntry| e.eval_id == eval_id && !e.in_flight;
            let row = if &active.0 == host && self.live.log.iter().any(closed) {
                Some((active.clone(), &mut self.live.log))
            } else {
                self.parked
                    .iter_mut()
                    .find(|(key, row)| {
                        **key != *active && &key.0 == host && row.log.iter().any(closed)
                    })
                    .map(|(key, row)| (key.clone(), &mut row.log))
            };
            let Some((key, log)) = row else { continue };
            if let Some(entry) = log.iter_mut().find(|e| closed(e)) {
                entry.in_flight = true;
                entry.frames.push(ReplFrame::Stderr {
                    text: LINK_BACK.to_string(),
                });
            }
            self.routes.insert((host.clone(), eval_id), key);
        }
        ReplEffects {
            redraw: true,
            ..Default::default()
        }
    }

    /// Control keys are already translated by State through RowDirectory/reply_ws_key.
    pub(in crate::ui) fn apply_frame(
        &mut self,
        host: HostKey,
        eval_id: u64,
        active: &WsKey,
        control_key: WsKey,
        frame: ReplFrame,
    ) -> ReplEffects {
        if let ReplFrame::Lifecycle { state } = frame {
            tracing::info!(host = %control_key.0, slug = %control_key.1, %state, "repl.frame: lifecycle");
            self.lifecycle.insert(control_key, state);
            return ReplEffects {
                listing: Some(host),
                redraw: true,
                ..Default::default()
            };
        }
        if let ReplFrame::Started {
            origin, display, ..
        } = frame
        {
            self.start_run(host, eval_id, active, control_key, origin, display);
            return ReplEffects {
                redraw: true,
                ..Default::default()
            };
        }
        if let ReplFrame::Browser { url, open, fe } = frame {
            return ReplEffects {
                browser: Some(BrowserAction {
                    host,
                    url,
                    open,
                    fe,
                }),
                redraw: true,
                ..Default::default()
            };
        }
        let done_elapsed = if let ReplFrame::Done { elapsed_ms, .. } = &frame {
            Some(*elapsed_ms)
        } else {
            None
        };
        let owner_id = (host, eval_id);
        let owner = self.routes.get(&owner_id).cloned();
        if let Some(entry) = self.routed_entry(owner.as_ref(), active, eval_id) {
            match frame {
                ReplFrame::Done { elapsed_ms, .. } => {
                    tracing::debug!(eval_id, elapsed_ms, "repl.frame: done (finalize)");
                    entry.elapsed_ms = elapsed_ms;
                    entry.in_flight = false;
                }
                other => {
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
        let mut effects = ReplEffects {
            redraw: true,
            ..Default::default()
        };
        if let Some(elapsed) = done_elapsed {
            self.routes.remove(&owner_id);
            if let Some((basename, project, fresh)) = self.runfile_status.remove(&owner_id) {
                effects.status = Some(run_status(
                    &basename,
                    project.as_deref(),
                    fresh,
                    Some(elapsed),
                ));
            }
        }
        effects
    }

    /// A session's Started announcement only registers a visited destination, and never duplicates ownership.
    fn start_run(
        &mut self,
        host: HostKey,
        eval_id: u64,
        active: &WsKey,
        key: WsKey,
        origin: String,
        display: String,
    ) {
        let owner_id = (host, eval_id);
        if self.routes.contains_key(&owner_id) {
            return;
        }
        let log = if key == *active {
            &mut self.live.log
        } else if let Some(row) = self.parked.get_mut(&key) {
            &mut row.log
        } else {
            tracing::debug!(eval_id, host = %key.0, slug = %key.1, "repl.frame: started for workspace with no snapshot — skipping");
            return;
        };
        if log.len() >= 256 {
            log.remove(0);
        }
        log.push(ReplEntry {
            eval_id,
            code: String::new(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight: true,
            pkg_mode: false,
            origin: Some(format!("{origin} ▸ {display}")),
        });
        self.routes.insert(owner_id, key);
    }

    pub(in crate::ui) fn apply_run_file_reply(
        &mut self,
        host: HostKey,
        eval_id: u64,
        active: &WsKey,
        result: Result<crate::net::transport::ReplRunFileInfo, String>,
    ) -> ReplEffects {
        let owner_id = (host, eval_id);
        let owner = self.routes.get(&owner_id).cloned();
        let status = match result {
            Ok(info) => {
                let basename = info
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&info.path)
                    .to_string();
                let acceptance = info.frames.is_empty() && info.elapsed_ms == 0;
                if acceptance {
                    self.runfile_status.insert(
                        owner_id,
                        (basename.clone(), info.project_dir.clone(), info.fresh),
                    );
                } else {
                    self.routes.remove(&owner_id);
                    if let Some(entry) = self.routed_entry(owner.as_ref(), active, eval_id) {
                        end_entry(entry, info.elapsed_ms, info.frames);
                    }
                }
                run_status(
                    &basename,
                    info.project_dir.as_deref(),
                    info.fresh,
                    (!acceptance).then_some(info.elapsed_ms),
                )
            }
            Err(message) => {
                tracing::warn!(error = %message, "repl.run_file failed");
                self.routes.remove(&owner_id);
                if let Some(entry) = self.routed_entry(owner.as_ref(), active, eval_id) {
                    entry.frames.push(ReplFrame::Error {
                        message: message.clone(),
                        stacktrace: Vec::new(),
                    });
                    entry.in_flight = false;
                }
                format!("repl.run_file failed · {message}")
            }
        };
        ReplEffects {
            status: Some(status),
            redraw: true,
            ..Default::default()
        }
    }
}

fn run_status(basename: &str, project: Option<&str>, fresh: bool, elapsed: Option<u64>) -> String {
    let context = if fresh {
        format!("fresh — project: {}", project.unwrap_or("(no project)"))
    } else {
        "existing repl".to_string()
    };
    match elapsed {
        Some(elapsed) => format!("ran '{basename}' ({context}, {elapsed}ms)"),
        None => format!("running '{basename}' ({context})…"),
    }
}

impl State {
    pub(crate) fn on_repl_eval_done(
        &mut self,
        host: HostKey,
        eval_id: u64,
        elapsed_ms: u64,
        frames: Vec<ReplFrame>,
    ) {
        let active = self.active_ws_key();
        self.repl_model
            .apply_eval_completion(host, eval_id, &active, elapsed_ms, frames);
    }

    pub(crate) fn on_repl_running(&mut self, host: &HostKey, ids: Vec<u64>) {
        let active = self.active_ws_key();
        let effects = self.repl_model.reopen_runs(host, &active, ids);
        self.apply_repl_effects(effects);
    }

    pub(crate) fn on_repl_frame_streamed(
        &mut self,
        host: HostKey,
        eval_id: u64,
        workspace_id: Option<String>,
        frame: ReplFrame,
    ) {
        let active = self.active_ws_key();
        let control = if matches!(&frame, ReplFrame::Lifecycle { .. }) {
            self.lifecycle_store_key(&host, workspace_id.as_deref())
        } else {
            (host.clone(), self.reply_ws_key(workspace_id.as_deref()))
        };
        let effects = self
            .repl_model
            .apply_frame(host, eval_id, &active, control, frame);
        self.apply_repl_effects(effects);
    }

    pub(crate) fn on_repl_run_file_done(
        &mut self,
        host: HostKey,
        eval_id: u64,
        result: Result<crate::net::transport::ReplRunFileInfo, String>,
    ) {
        let active = self.active_ws_key();
        let effects = self
            .repl_model
            .apply_run_file_reply(host, eval_id, &active, result);
        self.apply_repl_effects(effects);
    }

    pub(in crate::ui) fn apply_repl_effects(&mut self, effects: ReplEffects) {
        self.repl_presentation.apply_effects(&effects);
        if let Some(status) = effects.status {
            self.status.set(status);
        }
        if let Some(host) = effects.listing {
            let _ = self.send_to(&host, OutgoingReq::WorkspaceList);
        }
        if let Some(action) = effects.browser {
            self.open_repl_browser(action);
        }
        if effects.redraw {
            self.window.request_redraw();
        }
    }

    fn open_repl_browser(&mut self, action: BrowserAction) {
        let origin = crate::browser_open::origin_of(&action.url);
        if !crate::browser_open::opens_here(action.open, action.fe.as_deref(), &self_comm_handle())
        {
            tracing::info!(page = %origin, "wgl: browser frame served, not opened here");
            self.status.set("interactive figure served, not opened here".to_string());
            return;
        }
        if let Some(url) = self.ensure_proxy_for_url(
            &action.host,
            &action.url,
            crate::ui::page_proxy::PageSource::Announced,
        ) {
            match crate::browser_open::open_page(&url) {
                Ok(()) => self.status.set(format!("opened interactive figure · {origin}")),
                Err(e) => {
                    tracing::warn!(error = %e, page = %origin, "wgl: browser open failed");
                    self.status.set(format!("interactive figure · browser-open failed · {e}"));
                }
            }
        }
    }
}

#[cfg(all(test, feature = "test-window-progress"))]
#[path = "replies_native_fixture_tests.rs"]
mod native_fixture;
