//! Applying an `FeCommand` through the same methods the keys call.

use super::*;

/// True when an `op::FE_COMMAND` preview's `workspace` arg names the workspace
/// the FE is currently viewing — so the preview renders in place instead of
/// badging (fix for the dropped same-ws branch). `workspace` empty / "default"
/// / "<default>" means the daemon-default workspace, whose slug is
/// `default_slug`; the active view is `active_id` falling back to `default_slug`.
/// Pure so the same-ws decision is unit-testable without a live daemon/FE.
fn preview_targets_active_ws(
    active_id: Option<&str>,
    default_slug: Option<&str>,
    workspace: &str,
) -> bool {
    let is_default = workspace.is_empty() || workspace == "default" || workspace == "<default>";
    let target = if is_default {
        default_slug
    } else {
        Some(workspace)
    };
    let current = active_id.or(default_slug);
    target.is_some() && target == current
}

/// Which host a cross-workspace preview badge is filed under. `from_host` is
/// the connection that delivered the `FeCommand` — the daemon that actually
/// owns the target workspace — and wins whenever it's known; `active_host`
/// (whatever host the FE's view happened to be on when the badge arrived) is
/// only the fallback for a locally-originated dispatch (`from_host: None`).
/// Filing under `active_host` unconditionally was the bug: the view can move
/// to a different host between the badge and the later switch, and the
/// switch-time consume looks the entry up under the switched-TO
/// `active_host` — a badge keyed on the wrong host is never found. Pure so
/// the key choice is unit-testable without a live `State`.
fn badge_host_key(from_host: Option<&HostKey>, active_host: &HostKey) -> HostKey {
    from_host.cloned().unwrap_or_else(|| active_host.clone())
}

impl State {
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
    pub(in crate::ui) fn dispatch_fe_command(&mut self, from_host: Option<&HostKey>, cmd: FeCommand) {
        match cmd {
            FeCommand::Workspace { slug, boot } => {
                // null/empty/"default"/"<default>" → the daemon-default
                // workspace (active_workspace_id = None, keep current BL).
                // ADR 0042 L2a: the `fe.command` envelope names no host
                // (no protocol change in this slice) — resolved against
                // `active_host`, exactly the single-connection behavior
                // this command always had. Targeting another host is a
                // later slice's protocol addition.
                let slug = slug.filter(|s| !s.is_empty() && s != "default" && s != "<default>");
                if let Some(s) = slug.as_deref() {
                    if !self
                        .workspace_slugs
                        .iter()
                        .any(|(h, x)| h == &self.active_host && x == s)
                    {
                        tracing::warn!(slug = %s, "fe-command workspace: unknown slug, ignoring");
                        return;
                    }
                }
                let tmux = slug.as_ref().map(|s| format!("sot-be-{s}"));
                tracing::info!(?slug, boot, "fe-command: switch workspace");
                // Agent-driven, not a person looking: leave blue as-is.
                self.switch_to_workspace(self.active_host.clone(), slug, tmux, false);
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
                tracing::info!(%url, "fe-command: open_url");
                // The URL is loopback on the daemon that sent this command; a
                // command-file / internal dispatch names no host and gets the
                // default — the only proxied one anyway.
                let host = from_host.cloned().unwrap_or_else(|| self.default_host());
                if self.ensure_proxy_for_url(&host, &url) {
                    match open_url_in_browser(&url) {
                        Ok(()) => self.status = format!("opened in browser · {url}"),
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
                // ADR 0025 `preview --roi` (2026-07-21 update): arm the viewport
                // aim before routing — it rides preview's badge-floor routing
                // unchanged (aiming a viewport is MORE intrusive than previewing,
                // so no new focus-stealing) and is consumed by the render pass
                // once the aimed image is actually on screen: same-ws now,
                // cross-ws at badge-consume. One slot, latest-wins; a plain
                // re-preview of the same file retires a stale aim so it can't
                // fire on an old rect.
                let node_id = format!("files:{path}");
                match roi {
                    Some(rect) => {
                        self.pending_roi_aim = Some(RoiAim {
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
                            .is_some_and(|a| a.node_id == node_id)
                        {
                            self.pending_roi_aim = None;
                        }
                    }
                }
                // Figure caption: stored against the TARGET workspace (not the
                // active one) so a cross-ws badge carries its caption across the
                // switch that happens minutes later. Latest-wins per file, and
                // a caption-less re-preview of the same file retires the old one
                // — same staleness rule as the roi aim above, for the same
                // reason: a caption left over from a previous badge would
                // describe an image the agent has since replaced.
                let cap_ws_key = self.caption_ws_key(&workspace);
                match &caption {
                    Some(text) => {
                        self.preview_captions
                            .set(cap_ws_key, node_id.clone(), text.clone());
                    }
                    None => self.preview_captions.clear_one(&cap_ws_key, &node_id),
                }
                // Same-ws short-circuit: if the target workspace is the one we're
                // already viewing, render in place NOW (mirrors handle_nav_envelope
                // ui/control/envelope.rs, the in-place branch the imperative path dropped).
                // Without this, a same-ws preview badges + waits for a switch that
                // never comes (you're already there) — so a naive `sot-fe
                // preview <active-ws> <file>` opened nothing. The decision is a
                // pure fn (`preview_targets_active_ws`) so it's unit-tested.
                if preview_targets_active_ws(
                    self.active_workspace_id.as_deref(),
                    self.default_workspace_slug.as_deref(),
                    &workspace,
                ) {
                    tracing::info!(%workspace, %path, "fe-command: preview (same-ws, render in place)");
                    // Drive both panes: preview body + deep-path cursor reveal.
                    // The cursor follows the preview even when the file's
                    // ancestor dirs aren't expanded yet, so the nav header +
                    // viewport stay in sync (no more body-only / header mismatch).
                    self.drive_same_ws_open(&path);
                    return;
                }
                // Cross-workspace preview: badge by default, NEVER steal the
                // user's session (maintainer clarification 2026-07-10 PM,
                // revising the same morning's directive after living with
                // always-switch: "always set the nav and show means the file
                // should be selected in the nav and shown in preview, NOT to
                // yank my session over... I don't want to be yanked over mid
                // sentence"). The morning's actual bug was completeness — the
                // on-switch consume wasn't landing the nav cursor — which the
                // pending-nav reveal (#4 fix, switch_to_workspace) now does:
                // when the user visits the badged workspace, the file is
                // cursored in the nav AND rendered in the preview, always.
                //
                // `urgent` is the explicit user-requested "capture session
                // focus" option (sot-fe --urgent --fe <handle>): the route
                // layer only honors it on a DIRECTED send (broadcast urgent is
                // stripped — route_preview_urgent_is_directed_only), so a
                // blanket agent broadcast can never force-switch the view.
                if urgent {
                    tracing::info!(%workspace, %path, "fe-command: preview (user-requested focus capture)");
                    self.mark_pending_nav(
                        self.active_host.clone(),
                        workspace.clone(),
                        path.clone(),
                    );
                    let is_default =
                        workspace.is_empty() || workspace == "default" || workspace == "<default>";
                    let (slug, tmux) = if is_default {
                        (None, None)
                    } else {
                        (Some(workspace.clone()), Some(format!("sot-be-{workspace}")))
                    };
                    // Focus capture is honoring the AGENT's --urgent request,
                    // not a person switching the view: leave blue as-is.
                    self.switch_to_workspace(self.active_host.clone(), slug, tmux, false);
                } else {
                    // Badge floor: record + badge; the pending preview (body +
                    // nav-cursor reveal) is driven when the user next switches
                    // to `workspace` (see `switch_to_workspace`). Filed under
                    // the DELIVERING host (`badge_host_key`) — the daemon that
                    // owns `workspace` — not unconditionally `active_host`,
                    // which is only the view's host at arrival time and can
                    // differ from it (see `badge_host_key`'s doc comment).
                    let host = badge_host_key(from_host, &self.active_host);
                    tracing::info!(%workspace, %path, target_host = %host,
                        active_host = %self.active_host, "fe-command: preview (badge)");
                    self.mark_pending_nav(host, workspace, path);
                }
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
mod tests {
    use super::*;

    #[test]
    fn badge_host_key_prefers_the_delivering_host() {
        // A badge delivered from host A while the view sits on host B must
        // file under A -- the badge-key bug filed it under B (active_host),
        // so a later switch to A's workspace (which looks the entry up under
        // the switched-TO active_host, i.e. A once the switch lands) never
        // found it.
        let from_a: HostKey = "host-a".to_string();
        let active_b: HostKey = "host-b".to_string();
        assert_eq!(
            badge_host_key(Some(&from_a), &active_b),
            "host-a",
            "a known delivering host wins over the view's current host"
        );
        // A locally-originated dispatch (from_host: None) has no delivering
        // host to prefer, so it falls back to active_host -- the only case
        // the pre-fix code was actually correct for.
        assert_eq!(
            badge_host_key(None, &active_b),
            "host-b",
            "no delivering host known -> falls back to active_host"
        );
    }

    #[test]
    fn preview_same_ws_decision() {
        // Explicit slug == the active workspace -> render in place.
        assert!(preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "myanalysis"
        ));
        // Explicit slug != active -> NOT same-ws (force-show / badge path).
        assert!(!preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "ship_of_tools"
        ));
        // On the default workspace (active id None): targeting it by slug,
        // or by "default"/"<default>"/"" all resolve to same-ws.
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "ship_of_tools"
        ));
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "default"
        ));
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "<default>"
        ));
        assert!(preview_targets_active_ws(None, Some("ship_of_tools"), ""));
        // On a NON-default ws, targeting "default" is a real cross-ws switch,
        // not same-ws (so it must NOT short-circuit to in-place render).
        assert!(!preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "default"
        ));
        // No default slug known yet (pre-hello) + "default" target -> can't
        // resolve, so not same-ws (falls through to the safe badge path).
        assert!(!preview_targets_active_ws(None, None, "default"));
    }
}
