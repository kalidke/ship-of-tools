// Per-connection preview.changed fan-out filter (the flood fix): a
// connection must see exactly what its ACTIVATED workspace can use —
// same workspace tag, or a foreign-tagged event whose path is under the
// active root (workspaces overlap by design) — and nothing else, except
// before any `workspace.activate` has landed at all (send everything)
// and after an activated id stops resolving (send nothing until the
// next activate). Real on-disk roots throughout: `preview_changed_visible`
// resolves through `Workspace::files_mode()`, which canonicalizes
// (paths.rs `simplify_verbatim`) — exactly the "uncanonical stored root"
// failure mode this rework closes, so a literal `PathBuf` root would
// test nothing.
mod preview_changed_fanout {
    use super::super::preview_changed_visible;
    use crate::files::watcher::{ChangeKind, PreviewChanged};
    use crate::rows::{Workspace, Workspaces};
    use std::path::PathBuf;

    fn change(workspace_id: Option<&str>, path: PathBuf) -> PreviewChanged {
        PreviewChanged {
            path,
            node_id: Some("files:x".to_string()),
            kind: ChangeKind::Modified,
            workspace_id: workspace_id.map(str::to_string),
        }
    }

    /// A registry with one real, on-disk workspace named `slug`.
    /// Returns (registry, its CANONICAL workspace_id, its canonicalized
    /// root, the folder that holds the root: its drop removes it).
    fn registry_with_workspace(
        tag: &str,
        slug: &str,
    ) -> (Workspaces, String, PathBuf, tempfile::TempDir) {
        let held = tempfile::Builder::new()
            .prefix(&format!("sot-preview-fanout-{tag}-"))
            .tempdir()
            .unwrap();
        let dir = held.path().to_path_buf();
        let workspaces = Workspaces::new();
        let ws = workspaces.insert(Workspace::from_label(
            slug,
            dir,
            false,
            "none".to_string(),
            String::new(),
            String::new(),
        ));
        let root = ws.files_mode().unwrap().root_path().to_path_buf();
        (workspaces, ws.workspace_id.clone(), root, held)
    }

    #[test]
    fn same_workspace_tag_is_sent() {
        let (workspaces, id, _root, _held) = registry_with_workspace("tag", "alpha");
        // Trusted on the tag alone — the path doesn't even need to be
        // real or nearby.
        let c = change(Some("alpha"), PathBuf::from("/anywhere/at/all"));
        assert!(preview_changed_visible(&c, Some(&id), &workspaces));
    }

    #[test]
    fn foreign_tag_under_active_root_is_sent() {
        // The umbrella-workspace / watch-budget case: tagged "beta" but
        // the path is really inside the active workspace's own
        // (canonical, on-disk) tree.
        let (workspaces, id, root, _held) = registry_with_workspace("under", "alpha");
        let c = change(Some("beta"), root.join("src").join("main.jl"));
        assert!(preview_changed_visible(&c, Some(&id), &workspaces));
    }

    #[test]
    fn foreign_tag_elsewhere_is_dropped() {
        let (workspaces, id, _root, _held) = registry_with_workspace("elsewhere", "alpha");
        let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
        assert!(!preview_changed_visible(&c, Some(&id), &workspaces));
    }

    #[test]
    fn no_active_workspace_yet_sends_everything() {
        // Fresh connection, before its first `workspace.activate` —
        // today's (pre-filter) behaviour. Registry is irrelevant: `None`
        // short-circuits before it's ever consulted.
        let workspaces = Workspaces::new();
        let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
        assert!(preview_changed_visible(&c, None, &workspaces));
    }

    #[test]
    fn activated_id_no_longer_registered_drops_everything() {
        // The workspace was destroyed since this connection activated
        // it (or the activate itself named something that never
        // resolved) — drop, don't fall back to "send everything": the
        // registry has nothing to fall back TO.
        let workspaces = Workspaces::new();
        let c = change(Some("alpha"), PathBuf::from("/repos/alpha/src/main.jl"));
        assert!(!preview_changed_visible(
            &c,
            Some("ws-no-longer-exists"),
            &workspaces
        ));
    }
}

// `repl_frame_for`: a page every frontend would open goes out as it came to the window in use, and with
// `open: false` and no `fe` to every other connection, from one answer per frame. Every other frame goes out as it
// came.
mod browser_opener {
    use super::super::repl_frame_for;
    use crate::clients::Clients;
    use crate::sidecars::repl::ReplFrameMsg;
    use serde_json::{json, Value};

    fn msg(frame: &Value) -> ReplFrameMsg {
        ReplFrameMsg { eval_id: 1, workspace_id: Some("ws-x".into()), frame: frame.clone(), opener: Default::default() }
    }

    #[test]
    fn a_page_for_every_frontend_opens_in_the_window_in_use() {
        let clients = Clients::new();
        let fe_a = clients.register("a", "0.6.6", 1, "fe".into(), None, None, Some("fe@a".into()));
        let fe_b = clients.register("b", "0.6.6", 1, "fe".into(), None, None, Some("fe@b".into()));
        let (a, b) = (Some(fe_a.serial()), Some(fe_b.serial()));
        let page = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": true});
        let served = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": false});

        let m = msg(&page);
        assert_eq!(repl_frame_for(&m, b, &clients, true), page, "no window used: the one that connected last");
        assert_eq!(repl_frame_for(&m, a, &clients, true), served, "never every window");

        clients.touch_person_input(a.unwrap());
        let unset = json!({"kind": "browser", "url": "http://127.0.0.1:9/p"});
        let aimed = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": true, "fe": "fe@b"});
        for frame in [&page, &unset, &aimed] {
            let m = msg(frame);
            assert_eq!(repl_frame_for(&m, a, &clients, true), *frame, "the window used last, as it came");
            assert_eq!(repl_frame_for(&m, b, &clients, true), served, "{frame}");
            assert_eq!(repl_frame_for(&m, None, &clients, true), served, "{frame}");
        }

        let m = msg(&page);
        assert_eq!(repl_frame_for(&m, a, &clients, true), page);
        clients.touch_person_input(b.unwrap());
        assert_eq!(repl_frame_for(&m, b, &clients, true), served, "one answer per frame");
        assert_eq!(repl_frame_for(&msg(&page), b, &clients, true), page, "the next frame: the window used last now");

        let one = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": false, "fe": "fe@a"});
        let text = json!({"kind": "stdout", "text": "hi"});
        for frame in [&served, &one, &text] {
            assert_eq!(repl_frame_for(&msg(frame), a, &clients, true), *frame);
            assert_eq!(repl_frame_for(&msg(frame), b, &clients, true), *frame);
        }
    }
}

// A window that does not say it reads terminal escapes is sent the text of a frame with every sequence taken out, a
// window that does is sent it as the program printed it, and no other frame changes.
mod escapes_for_older_windows {
    use super::super::repl_frame_for;
    use crate::clients::Clients;
    use crate::sidecars::repl::ReplFrameMsg;
    use serde_json::json;

    fn written(frame: serde_json::Value, reads: bool) -> serde_json::Value {
        let m = ReplFrameMsg { eval_id: 1, workspace_id: None, frame, opener: Default::default() };
        repl_frame_for(&m, None, &Clients::new(), reads)
    }

    #[test]
    fn an_older_window_is_sent_no_escape_and_a_window_that_reads_them_is_sent_all() {
        let dirty = "a\u{1b}[31mred\u{1b}[39m \u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\\u{1b}(B\r\u{1b}[32m9 left\n";
        let clean = "ared link\r9 left\n";
        for (frame, field) in [
            (json!({"kind": "stdout", "text": dirty}), "text"),
            (json!({"kind": "stderr", "text": dirty}), "text"),
            (json!({"kind": "value", "mime": "text/plain", "text": dirty}), "text"),
            (json!({"kind": "error", "message": dirty, "stacktrace": []}), "message"),
        ] {
            assert_eq!(written(frame.clone(), true), frame, "a window that reads them: as it came");
            assert_eq!(written(frame.clone(), false)[field], clean, "{frame}");
            assert_eq!(written(frame.clone(), false)["kind"], frame["kind"]);
        }
        for frame in [json!({"kind": "done", "eval_id": 1, "elapsed_ms": 4}), json!({"kind": "image", "mime": "image/png", "data_base64": "\u{1b}[31m"})] {
            assert_eq!(written(frame.clone(), false), frame, "no other frame changes");
        }
    }
}

// What a window of v0.7.2 draws from the frames this daemon sends it: v0.7.2's own `strip_csi`, `cell_lines` and
// `stream_lines`, copied (`git show v0.7.2:rust/frontend/src/ui/chrome/status.rs` and `.../drawer/repl/presentation.rs`),
// run on what `repl_frame_for` writes for a window that does not read escapes, on three texts the child prints when an
// eval takes color. Fed the raw text, that window shows pieces of the sequences.
mod an_older_window_reads_clean_text {
    use super::super::repl_frame_for;
    use crate::clients::Clients;
    use crate::sidecars::repl::ReplFrameMsg;
    use serde_json::json;

    fn strip_csi(text: &str) -> String {
        let (mut out, mut rest) = (String::new(), text);
        while let Some(i) = rest.find("\u{1b}[") {
            out.push_str(&rest[..i]);
            let seq = &rest[i + 2..];
            let params = seq.find(|c| !matches!(c, '\u{30}'..='\u{3f}')).unwrap_or(seq.len());
            let end = seq[params..].find(|c| !matches!(c, '\u{20}'..='\u{2f}')).map_or(seq.len(), |k| params + k);
            if matches!(seq[end..].chars().next(), Some('\u{40}'..='\u{7e}')) {
                rest = &seq[end + 1..];
            } else {
                out.push('\u{1b}');
                rest = &rest[i + 1..];
            }
        }
        out.push_str(rest);
        out
    }

    fn cell_lines(text: &str) -> Vec<String> {
        strip_csi(text)
            .replace("\r\n", "\n")
            .replace(|c| matches!(c, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'), "\n")
            .lines()
            .map(|row| row.replace(char::is_control, " "))
            .collect()
    }

    fn stream_lines(text: &str) -> Vec<String> {
        text.replace("\r\n", "\n")
            .lines()
            .flat_map(|line| {
                let mut row: Vec<char> = Vec::new();
                for part in line.split('\r') {
                    for (i, c) in part.chars().enumerate() {
                        match row.get_mut(i) {
                            Some(cell) => *cell = c,
                            None => row.push(c),
                        }
                    }
                }
                let rows = cell_lines(&row.into_iter().collect::<String>());
                if rows.is_empty() { vec![String::new()] } else { rows }
            })
            .collect()
    }

    fn old_window_rows(frame: serde_json::Value, stream: bool) -> Vec<String> {
        let m = ReplFrameMsg { eval_id: 1, workspace_id: None, frame, opener: Default::default() };
        let sent = repl_frame_for(&m, None, &Clients::new(), false);
        let text = sent["text"].as_str().unwrap();
        if stream { stream_lines(text) } else { cell_lines(text) }
    }

    /// An ESC that starts no whole sequence (a program's own byte, the half of a sequence two writes split) is sent as the
    /// space v0.7.2's cell rule draws it as: no ESC byte reaches the connection, and the old window's rows are the same.
    #[test]
    fn an_esc_that_starts_no_whole_sequence_is_sent_as_the_space_v072_draws_it_as() {
        for raw in ["tail\u{1b}[31", "a\u{1b}[3", "1mred\u{1b}[0", "x\u{1b}", "x\u{1b}\u{1}y", "\u{1b}]8;;http://x", "p\u{1b}Xabc\u{1b}[3", "e\u{1b}\u{e9}"] {
            let frame = json!({"kind": "stdout", "text": raw});
            let m = ReplFrameMsg { eval_id: 1, workspace_id: None, frame, opener: Default::default() };
            let sent = repl_frame_for(&m, None, &Clients::new(), false)["text"].as_str().unwrap().to_string();
            assert!(!sent.contains('\u{1b}'), "{raw:?} sent as {sent:?}");
            assert_eq!(cell_lines(&sent), cell_lines(raw), "{raw:?}: the old window draws what it drew of the raw text");
            assert_eq!(stream_lines(&sent), stream_lines(raw), "{raw:?}");
        }
    }

    #[test]
    fn a_link_an_osc_and_a_repaint_show_as_a_terminal_shows_them() {
        // A Markdown value's link (`StyledStrings` writes OSC 8 once the stream says color).
        let link = "see \u{1b}]8;;http://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\ here";
        assert_eq!(old_window_rows(json!({"kind": "value", "mime": "text/plain", "text": link}), false), ["see link here"]);
        // A raw OSC 8 printed to stdout.
        let raw = "\u{1b}]8;;http://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\\n";
        assert_eq!(old_window_rows(json!({"kind": "stdout", "text": raw}), true), ["link"]);
        // A shrinking repaint: the old window applies `\r` before it strips, so on raw text it showed `9 leftm`.
        let repaint = "\r\u{1b}[32m10 left\u{1b}[39m\r\u{1b}[32m9 left\u{1b}[39m\n";
        assert_eq!(old_window_rows(json!({"kind": "stdout", "text": repaint}), true), ["9 leftt"]);
        assert_eq!(stream_lines(repaint), ["9 leftm"], "the raw text, which is what the probe guards against");
    }
}
