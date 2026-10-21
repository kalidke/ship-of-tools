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
    let text = |json: &str| serde_json::from_slice::<FeCommand>(json.as_bytes()).unwrap_err().to_string();
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
        Some(FeCommand::Workspace { slug }) => {
            assert_eq!(slug.as_deref(), Some("demo"));
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
