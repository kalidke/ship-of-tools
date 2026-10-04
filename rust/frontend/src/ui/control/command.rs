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
                boot: bool_arg("boot"),
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
                tracing::warn!(%url, "fe-command open_url: non-http(s) scheme refused");
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
        /// `--boot`: before switching, seed the target workspace's autostart
        /// flag so attach_session_to_bl boots ccb on attach — a scriptable
        /// spawn->goto->boot that's unconditional of what workspace.list has
        /// reported yet (sidesteps the registry-flag timing). Wire arg "boot".
        #[serde(default)]
        boot: bool,
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
    /// Open an arbitrary web URL in THIS machine's OS browser (maintainer note,
    /// 2026-07-05: "not port forward, FE opens"). http/https only —
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
mod tests {
    use super::*;

    // ─── ADR 0025: FE_COMMAND routing (target filter + cmd→FeCommand) ─────

    fn fe_evt(
        cmd: &str,
        args: serde_json::Value,
        target: Option<&str>,
    ) -> sot_protocol::ops::FeCommandEvt {
        sot_protocol::ops::FeCommandEvt {
            v: 1,
            cmd: cmd.to_string(),
            args,
            target: target.map(|s| s.to_string()),
            // Never reaches the wire (`#[serde(skip)]`) and irrelevant to
            // the FE's own `route_fe_command` — these tests exercise that
            // pure routing decision, not the daemon-side exclusive-delivery
            // filter (which lives entirely in server/events.rs).
            target_serial: None,
        }
    }

    #[test]
    fn route_target_none_acts_for_all_fes() {
        // target None = the badge floor: every FE acts.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            None,
        );
        let cmd = route_fe_command(&evt, "fe@host-a");
        assert!(matches!(cmd, Some(FeCommand::Preview { .. })));
    }

    #[test]
    fn route_target_self_acts() {
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("fe@host-a"),
        );
        let cmd = route_fe_command(&evt, "fe@host-a");
        assert!(matches!(cmd, Some(FeCommand::Preview { .. })));
    }

    #[test]
    fn route_target_other_is_ignored() {
        // Scoped to a different FE — we ignore it (it's force-show for someone
        // else).
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("fe@host-b"),
        );
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
    }

    #[test]
    fn route_preview_urgent_is_directed_only() {
        // DIRECTED (target == self) + urgent → urgent honoured (force-show
        // eligible; the idle gate still applies later in dispatch).
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            Some("fe@host-a"),
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview {
                workspace,
                path,
                urgent,
                roi,
                caption,
            }) => {
                assert_eq!(workspace, "ws");
                assert_eq!(path, "src/a.jl");
                assert_eq!(caption, None, "no caption arg → no caption");
                assert!(
                    urgent,
                    "directed urgent is carried through (force-show eligible)"
                );
                assert_eq!(roi, None, "no roi arg → no viewport aim");
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        // BROADCAST (target None = the badge floor) + urgent → urgent STRIPPED
        // to false. A broadcast must never force-show, or it would yank every
        // idle FE's view at once.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { urgent, .. }) => assert!(
                !urgent,
                "broadcast urgent is stripped — badge floor only, never force-show"
            ),
            other => panic!("expected Preview, got {other:?}"),
        }
        // urgent absent → false regardless of target.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("fe@host-a"),
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { urgent, .. }) => assert!(!urgent),
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    #[test]
    fn route_docs_parses_workspace_and_path() {
        // Backend-triggered docs.open: workspace + (absolute) path → FeCommand::Docs.
        // route_fe_command doesn't enforce absoluteness — the CLI does — so it
        // just threads both strings through.
        let evt = fe_evt(
            "docs",
            serde_json::json!({"workspace": "ws", "path": "/abs/site/index.html"}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Docs { workspace, path }) => {
                assert_eq!(workspace, "ws");
                assert_eq!(path, "/abs/site/index.html");
            }
            other => panic!("expected Docs, got {other:?}"),
        }
        // Missing path → None (required arg bails the parse).
        let evt = fe_evt("docs", serde_json::json!({"workspace": "ws"}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
    }

    #[test]
    fn route_reveal_maps_to_reveal_with_urgent() {
        // Directed so urgent is honoured (force-show is directed-only).
        let evt = fe_evt(
            "reveal",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            Some("fe@host-a"),
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Reveal {
                workspace,
                path,
                urgent,
                roi,
                caption,
            }) => {
                assert_eq!(workspace, "ws");
                assert_eq!(path, "src/a.jl");
                assert!(urgent);
                assert_eq!(roi, None);
                assert_eq!(caption, None);
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    /// Debug text with the `Reveal` variant name read as `Preview`, so a
    /// reveal and a preview of the same args compare equal on both sides of
    /// the alias merge.
    fn shown(x: impl std::fmt::Debug) -> String {
        format!("{x:?}").replacen("Reveal {", "Preview {", 1)
    }

    #[test]
    fn reveal_routes_as_preview() {
        let roi = serde_json::json!({"x": 10, "y": 20, "w": 300, "h": 200});
        let cases = [
            (serde_json::json!({"workspace": "ws", "path": "a.jl"}), Some("fe@host-a")),
            (
                serde_json::json!({"workspace": "ws", "path": "a.jl", "urgent": true}),
                Some("fe@host-a"),
            ),
            (serde_json::json!({"workspace": "ws", "path": "a.jl", "urgent": true}), None),
            (serde_json::json!({"workspace": "ws", "path": "a.png", "roi": roi}), None),
            (
                serde_json::json!({"workspace": "ws", "path": "a.png", "caption": "  one\n\ntwo\t "}),
                None,
            ),
        ];
        for (args, target) in cases {
            let via_reveal = route_fe_command(&fe_evt("reveal", args.clone(), target), "fe@host-a");
            let via_preview = route_fe_command(&fe_evt("preview", args.clone(), target), "fe@host-a");
            assert!(via_preview.is_some(), "{args}");
            assert_eq!(shown(via_reveal), shown(via_preview), "{args}");
        }
    }

    #[test]
    fn file_channel_reveal_parses_as_preview() {
        let fields = [
            r#""workspace":"ws","path":"a.jl""#,
            r#""workspace":"ws","path":"a.jl","urgent":true"#,
            r#""workspace":"ws","path":"a.png","roi":{"x":1,"y":2,"w":3,"h":4}"#,
            r#""workspace":"ws","path":"a.png","caption":"hi","urgent":false"#,
        ];
        for f in fields {
            let parse = |cmd: &str| {
                serde_json::from_str::<FeCommand>(&format!(r#"{{"cmd":"{cmd}",{f}}}"#)).unwrap()
            };
            assert_eq!(shown(parse("reveal")), shown(parse("preview")), "{f}");
        }
    }

    #[test]
    fn file_channel_error_texts_are_pinned() {
        let text = |json: &str| serde_json::from_str::<FeCommand>(json).unwrap_err().to_string();
        assert_eq!(text(r#"{"cmd":"nope"}"#), "unknown variant `nope`, expected one of `workspace`, `cycle_ws`, `reload_keybindings`, `notify`, `open_url`, `mode`, `nav`, `capture_roi`, `preview`, `reveal`, `docs`, `relaunch` at line 1 column 13");
        assert_eq!(text(r#"{"cmd":"reveal","workspace":"w"}"#), "missing field `path`");
    }

    #[test]
    fn file_channel_array_form_error_texts_are_pinned() {
        let text = |json: &str| serde_json::from_slice::<FeCommand>(json.as_bytes()).unwrap_err().to_string();
        assert_eq!(
            text(r#"["reveal","ws"]"#),
            "invalid length 1, expected struct variant FeCommand::Reveal with 5 elements"
        );
        assert_eq!(
            text(r#"["reveal"]"#),
            "invalid length 0, expected struct variant FeCommand::Reveal with 5 elements"
        );
        assert_eq!(
            text(r#"["preview","ws"]"#),
            "invalid length 1, expected struct variant FeCommand::Preview with 5 elements"
        );
        assert_eq!(
            text(r#"["preview"]"#),
            "invalid length 0, expected struct variant FeCommand::Preview with 5 elements"
        );
    }

    #[test]
    fn route_preview_parses_roi() {
        // Well-formed roi → carried through as a viewport aim.
        let evt = fe_evt(
            "preview",
            serde_json::json!({
                "workspace": "ws", "path": "out/plot.png",
                "roi": {"x": 10, "y": 20, "w": 300, "h": 200},
            }),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { roi, .. }) => assert_eq!(
                roi,
                Some(RoiRect {
                    x: 10,
                    y: 20,
                    w: 300,
                    h: 200
                })
            ),
            other => panic!("expected Preview, got {other:?}"),
        }
        // Malformed / zero-area / negative roi degrades to a PLAIN preview —
        // the command must never be dropped over its optional arg.
        for bad in [
            serde_json::json!("10,20,30,40"),
            serde_json::json!({"x": 1, "y": 2, "w": 0, "h": 5}),
            serde_json::json!({"x": -1, "y": 2, "w": 3, "h": 5}),
            serde_json::json!({"x": 1, "y": 2, "w": 3}),
        ] {
            let evt = fe_evt(
                "preview",
                serde_json::json!({"workspace": "ws", "path": "p.png", "roi": bad}),
                None,
            );
            match route_fe_command(&evt, "fe@host-a") {
                Some(FeCommand::Preview { roi, .. }) => {
                    assert_eq!(roi, None, "bad roi is ignored, not fatal")
                }
                other => panic!("expected Preview, got {other:?}"),
            }
        }
        // reveal carries roi too (the sot-fe CLI attaches it to either verb).
        let evt = fe_evt(
            "reveal",
            serde_json::json!({
                "workspace": "ws", "path": "out/plot.png",
                "roi": {"x": 0, "y": 0, "w": 1, "h": 1},
            }),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Reveal { roi, .. }) => assert_eq!(
                roi,
                Some(RoiRect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                })
            ),
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn route_preview_parses_caption() {
        let evt = fe_evt(
            "preview",
            serde_json::json!({
                "workspace": "ws", "path": "out/plot.png",
                "caption": "Recovery vs. SNR, 3 densities",
            }),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { caption, .. }) => {
                assert_eq!(caption.as_deref(), Some("Recovery vs. SNR, 3 densities"))
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        // reveal carries it too (the CLI attaches --caption to either verb).
        let evt = fe_evt(
            "reveal",
            serde_json::json!({"workspace": "ws", "path": "p.png", "caption": "hi"}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Reveal { caption, .. }) => {
                assert_eq!(caption.as_deref(), Some("hi"))
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn route_preview_caption_sanitizes_and_never_drops_the_command() {
        // Control chars (a heredoc-built caption arrives with newlines) collapse
        // to single spaces, and surrounding whitespace goes — a caption must not
        // be able to inject blank lines into the shaped band.
        let evt = fe_evt(
            "preview",
            serde_json::json!({
                "workspace": "ws", "path": "p.png",
                "caption": "  line one\n\nline two\ttabbed  ",
            }),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { caption, .. }) => {
                assert_eq!(caption.as_deref(), Some("line one line two tabbed"))
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        // An unusable caption degrades to a plain preview — losing the BADGE
        // over a bad caption would be strictly worse than losing the caption.
        for bad in [
            serde_json::json!(""),
            serde_json::json!("   \n\t  "),
            serde_json::json!(42),
            serde_json::json!({"text": "nope"}),
            serde_json::json!(null),
        ] {
            let evt = fe_evt(
                "preview",
                serde_json::json!({"workspace": "ws", "path": "p.png", "caption": bad}),
                None,
            );
            match route_fe_command(&evt, "fe@host-a") {
                Some(FeCommand::Preview { caption, path, .. }) => {
                    assert_eq!(caption, None, "bad caption is ignored, not fatal");
                    assert_eq!(path, "p.png", "the preview itself still happens");
                }
                other => panic!("expected Preview, got {other:?}"),
            }
        }
    }

    #[test]
    fn caption_truncates_by_chars_not_bytes() {
        // Under the cap: untouched.
        let short = "a".repeat(CAPTION_MAX_CHARS);
        assert_eq!(truncate_caption(&short), short);
        // Over the cap: cut to exactly the cap, last char an ellipsis.
        let long = "a".repeat(CAPTION_MAX_CHARS + 50);
        let cut = truncate_caption(&long);
        assert_eq!(cut.chars().count(), CAPTION_MAX_CHARS);
        assert!(cut.ends_with('…'));
        // Multi-byte input must cut on a CHARACTER boundary — a byte-offset
        // truncate would panic mid-codepoint here.
        let wide = "é".repeat(CAPTION_MAX_CHARS + 10);
        let cut = truncate_caption(&wide);
        assert_eq!(cut.chars().count(), CAPTION_MAX_CHARS);
        // The wire cap is enforced independently of the CLI's own cap, so a
        // non-CLI client can't seed an unbounded caption.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "p.png", "caption": "b".repeat(5000)}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Preview { caption, .. }) => {
                assert_eq!(caption.unwrap().chars().count(), CAPTION_MAX_CHARS)
            }
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    #[test]
    fn route_goto_workspace_maps_to_workspace() {
        let evt = fe_evt(
            "goto_workspace",
            serde_json::json!({"workspace": "demo"}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Workspace { slug, boot }) => {
                assert_eq!(slug.as_deref(), Some("demo"));
                assert!(!boot, "boot defaults false when arg absent");
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn route_goto_workspace_boot_flag() {
        // `sot-fe goto --boot <ws>` → args carry boot:true → FeCommand
        // seeds autostart before the switch (scriptable spawn->goto->boot).
        let evt = fe_evt(
            "goto_workspace",
            serde_json::json!({"workspace": "demo", "boot": true}),
            Some("fe@host-a"),
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Workspace { slug, boot }) => {
                assert_eq!(slug.as_deref(), Some("demo"));
                assert!(boot, "boot:true must thread through");
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn route_goto_mode_maps_to_mode() {
        let evt = fe_evt("goto_mode", serde_json::json!({"mode": "modules"}), None);
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Mode { mode }) => assert_eq!(mode, "modules"),
            other => panic!("expected Mode, got {other:?}"),
        }
    }

    #[test]
    fn route_notify_maps_with_optional_level() {
        let evt = fe_evt(
            "notify",
            serde_json::json!({"text": "build done", "level": "info"}),
            None,
        );
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Notify { text, level }) => {
                assert_eq!(text, "build done");
                assert_eq!(level.as_deref(), Some("info"));
            }
            other => panic!("expected Notify, got {other:?}"),
        }
        // level is optional.
        let evt = fe_evt("notify", serde_json::json!({"text": "hi"}), None);
        match route_fe_command(&evt, "fe@host-a") {
            Some(FeCommand::Notify { level, .. }) => assert!(level.is_none()),
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    #[test]
    fn route_unknown_cmd_is_ignored() {
        let evt = fe_evt("explode", serde_json::json!({}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
    }

    #[test]
    fn route_missing_required_arg_is_ignored() {
        // preview without path.
        let evt = fe_evt("preview", serde_json::json!({"workspace": "ws"}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
        // preview without workspace.
        let evt = fe_evt("preview", serde_json::json!({"path": "src/a.jl"}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
        // goto_workspace without workspace.
        let evt = fe_evt("goto_workspace", serde_json::json!({}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
        // notify without text.
        let evt = fe_evt("notify", serde_json::json!({"level": "info"}), None);
        assert!(route_fe_command(&evt, "fe@host-a").is_none());
    }
}
