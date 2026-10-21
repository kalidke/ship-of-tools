//! An fe.command evt's parse and route: `route_fe_command` and the `FeCommand` it yields.

use super::*;

/// Pure routing decision for an `FE_COMMAND` evt (ADR 0025): apply the target
/// filter, then map `(cmd, args)` to the `FeCommand` the dispatch sink runs.
/// `None` means "ignore" — for a bad target (scoped to another FE), an unknown
/// `cmd`, or a `cmd` missing a required arg. Pure + total so it's unit-testable;
/// the dispatch side effects (which need a `State`) are exercised separately.
///
/// Target filter: `None` → act (the badge floor; every FE acts). `Some(h)` →
/// act only when `h == self_handle` (force-show scoped to this FE); otherwise
/// ignore. The caller treats a `Some(h) == self_handle` match as force-show
/// eligible: `urgent` is carried through to `dispatch_fe_command`, which
/// honours it UNCONDITIONALLY on a directed send. There is no idle gate --
/// an earlier version of this comment named an `fe_is_idle` check that has
/// never existed in the code, and a session chasing a preview that did not
/// switch reasoned from it that `--urgent` must be inert for exactly the
/// frontend someone is typing at. Directedness is the whole gate.
#[allow(clippy::too_many_lines, reason = "routes one FE command by its target filter and verb; predates the 100-line limit")]
pub(in crate::ui) fn route_fe_command(evt: &sot_protocol::ops::FeCommandEvt, self_handle: &str) -> Option<FeCommand> {
    // Target filter first — cheapest reject, and a mis-targeted command
    // shouldn't even be parsed.
    if let Some(h) = evt.target.as_deref() {
        if h != self_handle {
            return None;
        }
    }
    let args = &evt.args;
    let str_arg = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let bool_arg = |key: &str| args.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    // Force-show is DIRECTED-only. A broadcast (target None = the badge floor)
    // must NEVER force-show — an `urgent` broadcast would otherwise yank EVERY
    // idle FE's view at once. `urgent` is honoured only when the command is
    // targeted to THIS FE (target Some == self, having passed the filter
    // above); a broadcast collapses to the non-disruptive badge regardless of
    // the `urgent` arg.
    let directed = evt.target.is_some();
    // Optional `roi = {x,y,w,h}` (source-image px, ADR-0022 vocabulary; ADR
    // 0025 2026-07-21 update). Malformed or zero-area → ignored, plain
    // preview: the request side ships ahead of consumers and must degrade
    // gracefully, never drop the command.
    let roi_arg = || {
        args.get("roi")
            .and_then(|v| match serde_json::from_value::<RoiRect>(v.clone()) {
                Ok(r) if r.w > 0 && r.h > 0 => Some(r),
                Ok(_) => {
                    tracing::warn!("fe-command preview: zero-area roi ignored");
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "fe-command preview: unparsable roi ignored");
                    None
                }
            })
    };
    // Optional `caption` — agent-authored prose drawn under the image. Same
    // degrade-never-drop contract as `roi`: an unusable caption is dropped, the
    // preview still happens. Sanitized here rather than at render time so the
    // stored value is already safe for every consumer: control chars (including
    // the newlines a heredoc-built caption picks up) collapse to spaces so a
    // caption can't smuggle in blank lines or break the shaped layout, and the
    // length is capped independently of the CLI (the wire is reachable without
    // it — the ADR-0019 file channel and any other daemon client).
    let caption_arg = || {
        let raw = args.get("caption")?.as_str()?;
        let cleaned = raw
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>();
        let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return None;
        }
        Some(truncate_caption(&collapsed))
    };
    match evt.cmd.as_str() {
        "preview" => {
            let workspace = str_arg("workspace")?;
            let path = str_arg("path")?;
            Some(FeCommand::Preview {
                workspace,
                path,
                urgent: bool_arg("urgent") && directed,
                roi: roi_arg(),
                caption: caption_arg(),
            })
        }
        "reveal" => {
            let workspace = str_arg("workspace")?;
            let path = str_arg("path")?;
            Some(FeCommand::Reveal {
                workspace,
                path,
                urgent: bool_arg("urgent") && directed,
                roi: roi_arg(),
                caption: caption_arg(),
            })
        }
        "goto_workspace" => {
            let workspace = str_arg("workspace")?;
            Some(FeCommand::Workspace {
                slug: Some(workspace),
            })
        }
        "goto_mode" => {
            let mode = str_arg("mode")?;
            Some(FeCommand::Mode { mode })
        }
        "notify" => {
            let text = str_arg("text")?;
            Some(FeCommand::Notify {
                text,
                level: str_arg("level"),
            })
        }
        "open_url" => {
            let url = str_arg("url")?;
            // Scheme allowlist: a BE-relayed command must not be able to
            // launch arbitrary local handlers (file:, javascript:, custom
            // protocol hijacks). Browsers own http/https; nothing else.
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                let origin = crate::browser_open::origin_of(&url);
                tracing::warn!(scheme = %origin, "fe-command open_url: non-http(s) scheme refused");
                return None;
            }
            Some(FeCommand::OpenUrl { url })
        }
        "docs" => {
            // Backend-triggered docs.open for a local .html/site. `path` is the
            // ABSOLUTE backend fs path (docs.open stats it raw + confines it to
            // a workspace root); no scheme allowlist — it never opens a URL, it
            // serves a confined local file over the site_serve port.
            let workspace = str_arg("workspace")?;
            let path = str_arg("path")?;
            Some(FeCommand::Docs { workspace, path })
        }
        "relaunch" => {
            if !directed {
                tracing::warn!("fe-command relaunch: broadcast refused; target one frontend with --fe");
                return None;
            }
            Some(FeCommand::Relaunch {
                converge: bool_arg("converge"),
            })
        }
        _ => None,
    }
}

/// A control command raised through the FE command-file channel (ADR 0019).
/// One JSON object per file under `fe-commands/`, internally tagged by `cmd`.
/// The watcher parses into this enum and the main thread dispatches each
/// through the same methods the keybinds use. New variants land here as later
/// ADR-0019 commits add command groups (reload_keybindings, notify, mode, nav).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub(in crate::ui) enum FeCommand {
    /// Switch to a named workspace by slug. A null/absent/"default" slug
    /// resolves to the daemon-default workspace (`active_workspace_id = None`).
    Workspace {
        #[serde(default)]
        slug: Option<String>,
    },
    /// Cycle the active workspace by `dir` (+1 next, -1 prev); wraps. Defaults
    /// to +1 so `{"cmd":"cycle_ws"}` means "next".
    CycleWs {
        #[serde(default = "fe_cmd_default_dir")]
        dir: i32,
    },
    /// Re-read the layered keybindings config live — for after an
    /// in-terminal agent edits `.sot/keybindings.toml` (or another layer).
    ReloadKeybindings,
    /// Surface a text message in the chrome status line. `level` is advisory
    /// (info|warn|error) and rendered uniformly for now.
    Notify {
        text: String,
        #[serde(default)]
        level: Option<String>,
    },
    /// Open an arbitrary web URL in THIS machine's OS browser, not through a port forward
    /// (decided 2026-07-05). http/https only —
    /// enforced at route time so a hostile scheme never reaches dispatch.
    OpenUrl { url: String },
    /// Switch nav mode: "files" | "modules" | "sessions" | "hosts".
    Mode { mode: String },
    /// Drive the nav cursor/tree: "up" | "down" | "expand" | "collapse" |
    /// "pin". Cursor *activation* (keyboard Enter, with its Sessions/Hosts
    /// side effects) stays keyboard-only — the `workspace` command already
    /// covers the session-switch case.
    Nav { action: String },
    /// ADR 0022: capture the visible image-preview ROI and send it to the LLM
    /// pane. Lets the in-pane agent trigger a capture (`{"cmd":"capture_roi"}`)
    /// without the `C` hotkey — the "look at what I'm zoomed into" pull path.
    /// No-op (status hint) when the preview isn't a croppable image.
    CaptureRoi,
    /// ADR 0025 imperative preview: show `path` (workspace-relative) in
    /// `workspace`'s Files-mode preview. `urgent` requests force-show (switch +
    /// show now) and is honoured whenever the send is DIRECTED; without it a
    /// send degrades to the badge floor (`mark_pending_nav`), the
    /// non-disruptive default. No idle condition enters this -- see
    /// `route_fe_command`. Carried as
    /// an `FE_COMMAND` evt's `{cmd:"preview", args:{workspace, path, urgent?,
    /// roi?}}`; also constructible from the ADR-0019 file channel for parity.
    /// `roi` (ADR 0025 2026-07-21 update) is an optional source-px viewport
    /// aim, solved into zoom/pan FE-side, clamped, and echoed back via the
    /// `preview_roi_applied` event.
    /// `caption` is an optional agent-authored figure caption drawn under the
    /// image (images only). Unlike `roi` — a one-shot aim consumed by the next
    /// matching render — a caption is STICKY per (workspace, file): it outlives
    /// the badge, the workspace switch, and re-previews, because the whole point
    /// is that it's still there when the user arrives. `None` retires the
    /// caption for that file (see `dispatch_fe_command`).
    Preview {
        workspace: String,
        path: String,
        #[serde(default)]
        urgent: bool,
        #[serde(default)]
        roi: Option<RoiRect>,
        #[serde(default)]
        caption: Option<String>,
    },
    /// ADR 0025 imperative reveal — dispatched as `Preview` (badge floor /
    /// force-show + on-switch preview, `roi` carried through). The
    /// same-workspace preview path also does the deep tree-expand-and-select
    /// of the target row (`drive_same_ws_open`, nav/files/reveal.rs), so
    /// `reveal` and `preview` both move the nav cursor onto the file.
    Reveal {
        workspace: String,
        path: String,
        #[serde(default)]
        urgent: bool,
        #[serde(default)]
        roi: Option<RoiRect>,
        #[serde(default)]
        caption: Option<String>,
    },
    /// ADR 0025 imperative `docs.open` (the `W` key) triggered from the backend:
    /// serve a local `.html`/site from backend disk over the forwarded
    /// site_serve port and open it in the FE's real browser. `path` is an
    /// ABSOLUTE backend-side fs path — `docs.open` stats it raw and confines it
    /// to ANY registered workspace, so no workspace switch is needed and
    /// `workspace` is carried for parity/logging only (the backend ignores it).
    Docs { workspace: String, path: String },
    /// ADR 0017 self-relaunch, requested over the command channel: exit 75
    /// (plain respawn) or 76 (`converge`: pull + rebuild + daemon-pair
    /// re-ensure, then respawn). DIRECTED only -- a broadcast would bounce
    /// every attached frontend at once.
    Relaunch { converge: bool },
}

fn fe_cmd_default_dir() -> i32 {
    1
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
