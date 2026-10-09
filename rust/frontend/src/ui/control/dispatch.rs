//! Applying an `FeCommand` through the same methods the keys call.

use super::*;

pub(super) enum ResultRoute {
    Refusal(String),
    Render(ResolvedWorkspace),
    Badge(ResolvedWorkspace),
    Switch(ResolvedWorkspace),
}

fn result_route_decision(
    lists: &HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>>,
    host: &HostKey,
    spelling: &str,
    active: Option<&ResultRowIdentity>,
    urgent: bool,
    goto: bool,
) -> ResultRoute {
    let target = match resolve_listed_workspace(lists, host, spelling) {
        Ok(target) => target,
        Err(reason) => return ResultRoute::Refusal(reason),
    };
    let same_row = active == Some(target.identity());
    if !goto && same_row {
        return ResultRoute::Render(target);
    }
    if !goto && !target.visible() {
        return ResultRoute::Refusal("result target has no visible session row".to_string());
    }
    if goto || urgent {
        if let Err(reason) = target.attachment() {
            return ResultRoute::Refusal(reason);
        }
        ResultRoute::Switch(target)
    } else {
        ResultRoute::Badge(target)
    }
}

impl State {
    pub(super) fn result_route(
        &self,
        host: &HostKey,
        spelling: &str,
        urgent: bool,
        goto: bool,
    ) -> ResultRoute {
        let active = self.active_result_workspace();
        result_route_decision(
            &self.workspace_lists,
            host,
            spelling,
            active.as_ref().map(ResolvedWorkspace::identity),
            urgent,
            goto,
        )
    }

    pub(in crate::ui) fn refuse_result(&mut self, reason: &str) {
        self.status = format!("result refused · {reason}");
        tracing::warn!(%reason, "result target refused");
        self.window.request_redraw();
    }

    /// Drain commands the FE-control watcher (ADR 0019) enqueued and dispatch
    /// each on the main thread. Called near the top of `window_event` after a
    /// `request_redraw` wake; a cheap no-op when the queue is empty. The lock
    /// is scoped to the drain so dispatch (which mutates `self`) doesn't hold
    /// it.
    pub(in crate::ui) fn drain_fe_commands(&mut self) {
        let cmds: Vec<FeCommand> = match self.fe_commands.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return,
        };
        for cmd in cmds {
            self.dispatch_fe_command(None, cmd);
        }
    }

    /// Apply one FE-control command, reusing the methods the keybinds call so
    /// commands inherit the same routing (incl. the ADR-0014 per-workspace
    /// tree-reply guard).
    #[allow(clippy::too_many_lines, reason = "the FE-control command table: one arm per command; predates the 100-line limit")]
    pub(in crate::ui) fn dispatch_fe_command(&mut self, from_host: Option<&HostKey>, cmd: FeCommand) {
        match cmd {
            FeCommand::Workspace { slug, boot } => {
                let host = from_host.unwrap_or(&self.active_host).clone();
                match self.result_route(&host, slug.as_deref().unwrap_or(""), true, true) {
                    ResultRoute::Switch(target) => self.switch_to_resolved_workspace(target, false),
                    ResultRoute::Refusal(reason) => self.refuse_result(&reason),
                    _ => unreachable!("goto always switches a resolved target"),
                }
                let _ = boot;
            }
            FeCommand::CycleWs { dir } => {
                let dir = if dir == 0 { 1 } else { dir };
                tracing::info!(dir, "fe-command: cycle workspace");
                // Command-file driven, not a person looking: leave blue as-is
                // (2026-09-08 review, finding 3 — this was the forgeable path).
                self.cycle_workspace(dir, false);
            }
            FeCommand::ReloadKeybindings => {
                self.bindings = KeyBindings::load_layered();
                tracing::info!("fe-command: reloaded keybindings");
                self.status = "keybindings reloaded".to_string();
                self.window.request_redraw();
            }
            FeCommand::Notify { text, level } => {
                tracing::info!(?level, %text, "fe-command: notify");
                self.status = text;
                // Pin it briefly so a workspace switch doesn't instantly rebuild
                // the status line over it (see NOTIFY_STICKY).
                self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                self.window.request_redraw();
            }
            FeCommand::OpenUrl { url } => {
                // Scheme already allowlisted (http/https) at route time.
                let origin = crate::browser_open::origin_of(&url);
                tracing::info!(page = %origin, "fe-command: open_url");
                // The URL is loopback on the daemon that sent this command; a
                // command-file / internal dispatch names no host and gets the
                // default — the only proxied one anyway.
                let host = from_host.cloned().unwrap_or_else(|| self.default_host());
                if let Some(url) = self.ensure_proxy_for_url(&host, &url, crate::ui::page_proxy::PageSource::Announced) {
                    match crate::browser_open::open_page(&url) {
                        Ok(()) => self.status = format!("opened in browser · {origin}"),
                        Err(e) => self.status = format!("open_url failed · {e}"),
                    }
                }
                self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                self.window.request_redraw();
            }
            FeCommand::Mode { mode } => {
                let m = match mode.as_str() {
                    "files" => Some(Mode::Files),
                    "modules" => Some(Mode::Modules),
                    "sessions" => Some(Mode::Sessions),
                    "hosts" => Some(Mode::Hosts),
                    _ => None,
                };
                match m {
                    Some(m) => {
                        tracing::info!(%mode, "fe-command: mode");
                        self.enter_mode(m);
                    }
                    None => {
                        tracing::warn!(%mode, "fe-command mode: unknown mode, ignoring");
                    }
                }
            }
            FeCommand::Nav { action } => {
                tracing::info!(%action, "fe-command: nav");
                match action.as_str() {
                    "down" => self.tree.move_down(),
                    "up" => self.tree.move_up(),
                    "expand" => {
                        self.try_expand_selected();
                    }
                    "collapse" => {
                        if !self.collapse_selected_row() {
                            if let Some(p) = self.tree.parent_of_selected() {
                                self.tree.selected = p;
                            }
                        }
                    }
                    "pin" => self.toggle_pin(),
                    other => {
                        tracing::warn!(action = %other, "fe-command nav: unknown action, ignoring");
                        return;
                    }
                }
                self.maybe_fire_preview();
                self.window.request_redraw();
            }
            FeCommand::CaptureRoi => {
                tracing::info!("fe-command: capture_roi");
                self.capture_roi();
            }
            FeCommand::Preview {
                workspace,
                path,
                urgent,
                roi,
                caption,
            } => {
                let host = from_host.unwrap_or(&self.active_host).clone();
                let route = self.result_route(&host, &workspace, urgent, false);
                let target = match &route {
                    ResultRoute::Refusal(reason) => {
                        self.refuse_result(reason);
                        return;
                    }
                    ResultRoute::Render(target)
                    | ResultRoute::Badge(target)
                    | ResultRoute::Switch(target) => target,
                };
                let key = target.row_key().clone();
                let node_id = format!("files:{path}");
                match roi {
                    Some(rect) => {
                        self.pending_roi_aim = Some(RoiAim {
                            row_key: key.clone(),
                            workspace: workspace.clone(),
                            path: path.clone(),
                            node_id: node_id.clone(),
                            rect,
                            ready: false,
                        });
                    }
                    None => {
                        if self
                            .pending_roi_aim
                            .as_ref()
                            .is_some_and(|aim| aim.row_key == key && aim.node_id == node_id)
                        {
                            self.pending_roi_aim = None;
                        }
                    }
                }
                match caption {
                    Some(text) => self.preview_captions.set(key, node_id, text),
                    None => self.preview_captions.clear_one(&key, &node_id),
                }
                match route {
                    ResultRoute::Render(_) => self.drive_same_ws_open(&path),
                    ResultRoute::Badge(target) => {
                        let (host, slug) = target.row_key().clone();
                        self.mark_pending_nav(host, slug, path);
                    }
                    ResultRoute::Switch(target) => {
                        let (host, slug) = target.row_key().clone();
                        self.mark_pending_nav(host, slug, path);
                        self.switch_to_resolved_workspace(target, false);
                    }
                    ResultRoute::Refusal(_) => unreachable!("refusal checked before effects"),
                }
            }
            FeCommand::Reveal {
                workspace,
                path,
                urgent,
                roi,
                caption,
            } => {
                // reveal == preview: the same-ws preview path now performs the
                // deep tree-expand-and-select (cursor follows the file, ancestor
                // dirs expand async), so `reveal` and `preview` both drive the
                // nav cursor onto the file — the BE need not pick the right verb
                // or issue a separate cursor move. The cross-ws force-show/badge
                // semantics are shared too, as is a `--roi` viewport aim (the
                // sot-fe CLI attaches roi to either verb).
                self.dispatch_fe_command(from_host, FeCommand::Preview {
                    workspace,
                    path,
                    urgent,
                    roi,
                    caption,
                });
            }
            FeCommand::Relaunch { converge } => {
                // Same exit codes the sentinel watcher produces; the supervisor
                // (launch-sot.ps1 / sot-launch) respawns us. `ephemeral` FEs
                // (`--capture`) have no supervisor and must not exit for one.
                if self.ephemeral {
                    self.status = "relaunch refused · ephemeral frontend".to_string();
                } else {
                    tracing::info!(converge, "fe-command: relaunch");
                    self.status = if converge { "converging…" } else { "relaunching…" }.to_string();
                    self.relaunch_flag.store(
                        if converge { 76 } else { 75 },
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                self.window.request_redraw();
            }
            FeCommand::Docs { workspace, path } => {
                // No workspace switch: docs.open confines the absolute path to
                // ANY registered workspace on the backend, so the active FE
                // workspace is irrelevant. This is exactly what the `W` key does,
                // minus the keypress — the FE's own long-lived connection mints
                // the site_serve nonce, so the URL outlives the send (a backend
                // one-shot docs.open would have its nonce reaped on disconnect).
                tracing::info!(%workspace, %path, "fe-command: docs");
                self.docs_open_external(path);
            }
        }
    }
}

#[cfg(test)]
mod result_route_tests {
    use super::*;

    #[test]
    fn unlisted_ambiguous_and_inert_results_refuse_before_effects() {
        let host = "<host>".to_string();
        let mut lists = HashMap::new();
        assert!(matches!(
            result_route_decision(&lists, &host, "project", None, true, false),
            ResultRoute::Refusal(_)
        ));
        let mut row = ws_info("anchor", "anchor-target");
        row.is_default = true;
        row.agent = "none".into();
        lists.insert(host.clone(), vec![row.clone()]);
        for alias in ["", "default", "<default>", "anchor"] {
            assert!(matches!(
                result_route_decision(&lists, &host, alias, None, false, false),
                ResultRoute::Refusal(_)
            ));
            let target = resolve_listed_workspace(&lists, &host, alias).unwrap();
            assert!(matches!(
                result_route_decision(&lists, &host, alias, Some(target.identity()), false, false),
                ResultRoute::Render(_)
            ));
            assert!(matches!(
                result_route_decision(&lists, &host, alias, None, true, true),
                ResultRoute::Switch(_)
            ));
        }
        row.agent = "claude".into();
        row.session_name.clear();
        lists.insert(host.clone(), vec![row.clone()]);
        assert!(matches!(
            result_route_decision(&lists, &host, "anchor", None, false, false),
            ResultRoute::Badge(_)
        ));
        assert!(matches!(
            result_route_decision(&lists, &host, "anchor", None, true, true),
            ResultRoute::Refusal(_)
        ));
        lists.get_mut(&host).unwrap().push(row);
        assert!(matches!(
            result_route_decision(&lists, &host, "anchor", None, true, false),
            ResultRoute::Refusal(_)
        ));
    }
}
