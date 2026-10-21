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
        assert_eq!(repl_frame_for(&m, b, &clients), page, "no window used: the one that connected last");
        assert_eq!(repl_frame_for(&m, a, &clients), served, "never every window");

        clients.touch_person_input(a.unwrap());
        let unset = json!({"kind": "browser", "url": "http://127.0.0.1:9/p"});
        let aimed = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": true, "fe": "fe@b"});
        for frame in [&page, &unset, &aimed] {
            let m = msg(frame);
            assert_eq!(repl_frame_for(&m, a, &clients), *frame, "the window used last, as it came");
            assert_eq!(repl_frame_for(&m, b, &clients), served, "{frame}");
            assert_eq!(repl_frame_for(&m, None, &clients), served, "{frame}");
        }

        let m = msg(&page);
        assert_eq!(repl_frame_for(&m, a, &clients), page);
        clients.touch_person_input(b.unwrap());
        assert_eq!(repl_frame_for(&m, b, &clients), served, "one answer per frame");
        assert_eq!(repl_frame_for(&msg(&page), b, &clients), page, "the next frame: the window used last now");

        let one = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": false, "fe": "fe@a"});
        let text = json!({"kind": "stdout", "text": "hi"});
        for frame in [&served, &one, &text] {
            assert_eq!(repl_frame_for(&msg(frame), a, &clients), *frame);
            assert_eq!(repl_frame_for(&msg(frame), b, &clients), *frame);
        }
    }
}
