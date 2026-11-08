// The setter map is process-wide, so these tests take their serials from 9_00x_xxx and give each row an id of its own:
// a test that reused a real roster's serial or another test's row id would remove that test's files.
use super::*;
use crate::agents::selection_check::FileText;
use sot_protocol::ops::TextPos;
use sot_protocol::selection_digest;

fn sel(node: &str, start: (u32, u32), end: (u32, u32), shown: &str) -> SelectedText {
    SelectedText {
        node_id: node.to_string(),
        start: TextPos { line: start.0, byte: start.1 },
        end: TextPos { line: end.0, byte: end.1 },
        len: shown.len() as u32,
        digest: selection_digest(shown),
    }
}

fn req(workspace: &str, selection: Option<SelectedText>) -> SelectionSetReq {
    SelectionSetReq { workspace_id: workspace.to_string(), selection, drawing: None }
}

#[test]
fn the_row_file_holds_the_last_shared_selection_or_nothing() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    let mut row = Workspace::from_label("sel", project.path().to_path_buf(), false, "claude".into(), String::new(), String::new());
    row.workspace_id = "ws-sel-1".to_string();
    let ws = workspaces.insert(row);
    workspaces.set_default(&ws.workspace_id);
    let root = ws.files_mode().unwrap().root_path().to_path_buf();
    let folder = state_dir_for(state.path(), &ws.workspace_id);
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join(FILE_NAME);
    let held = || std::fs::read_to_string(&file).ok();
    let set = |workspace: &str, s: Option<SelectedText>| set_selection(Some(state.path()), &workspaces, true, 9_001_001, &req(workspace, s));
    // the rendering of lines `first` to `last` of `name`: the absolute path and the project-relative path differ
    let expect = |name: &str, text: &str, first: usize, last: usize| {
        let absolute = root.join(name).to_string_lossy().into_owned();
        let f = FileText { text: text.to_string(), first_line: first, last_line: last };
        render_selection(&f, &absolute, name)
    };
    std::fs::write(project.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(project.path().join("b.txt"), "one\ntwo\n").unwrap();
    std::fs::write(project.path().join("bin.dat"), b"ab\xffcd\n").unwrap();
    std::fs::write(outside.path().join("out.txt"), "secret\nlines\n").unwrap();

    // 1. an accepted selection publishes exactly the rendering of the file's own text
    assert_eq!(set(&ws.workspace_id, Some(sel("files:a.txt", (0, 2), (1, 2), "pha\nbe"))), Ok(SelectionState::Held));
    assert_eq!(held(), Some(expect("a.txt", "pha\nbe", 1, 2)), "row 1: an accepted selection publishes the file");
    // the file is JSON, so a Windows path's backslashes are escaped in it: compare the decoded text the agent reads
    let doc: serde_json::Value = serde_json::from_str(&held().unwrap()).unwrap();
    let context = doc["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(context.contains(&root.join("a.txt").to_string_lossy().into_owned()), "row 1: the absolute path");
    assert_eq!(doc["systemMessage"], "Selection: a.txt lines 1-2", "row 1: the relative path");

    // 2. a second accepted selection replaces the first
    assert_eq!(set(&ws.workspace_id, Some(sel("files:b.txt", (1, 0), (1, 3), "two"))), Ok(SelectionState::Held));
    assert_eq!(held(), Some(expect("b.txt", "two", 2, 2)), "row 2: a second selection replaces the first");

    // 3. a clear removes the file
    assert_eq!(set(&ws.workspace_id, None), Ok(SelectionState::Shared));
    assert_eq!(held(), None, "row 3: a clear removes the file");

    // 4. a refusal removes an earlier file, and the file outside the root is never read
    let refusals: Vec<(&str, SelectedText, &str)> = vec![
        ("not the file's text", sel("files:a.txt", (0, 0), (0, 5), "alphX"), selection_code::NOT_FILE_TEXT),
        ("not UTF-8", sel("files:bin.dat", (0, 0), (0, 5), "ab\u{fffd}cd"), selection_code::NOT_FILE_TEXT),
        ("a folder", sel("files:", (0, 0), (0, 1), "x"), selection_code::NOT_FILE_TEXT),
        ("a missing file", sel("files:none.txt", (0, 0), (0, 1), "x"), selection_code::NOT_FILE_TEXT),
        ("an empty id", sel("", (0, 0), (0, 5), "alpha"), selection_code::BAD_NODE_ID),
        ("no prefix", sel("a.txt", (0, 0), (0, 5), "alpha"), selection_code::BAD_NODE_ID),
        (
            "a parent segment out of the root",
            sel(&format!("files:../{}/out.txt", outside.path().file_name().unwrap().to_string_lossy()), (0, 0), (0, 6), "secret"),
            selection_code::BAD_NODE_ID,
        ),
        (
            "an absolute path",
            sel(&format!("files:{}/out.txt", outside.path().display()), (0, 0), (0, 6), "secret"),
            selection_code::BAD_NODE_ID,
        ),
    ];
    for (name, s, code) in refusals {
        assert!(set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"))).is_ok(), "setup for {name}");
        assert!(held().is_some(), "setup for {name}");
        assert_eq!(set(&ws.workspace_id, Some(s)), Err(code), "row 4: {name}");
        assert_eq!(held(), None, "row 4: {name} removes the earlier file");
    }

    // 4b. a link out of the root is followed as the preview follows it: the file's own text is shared
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("out.txt"), project.path().join("link.txt")).unwrap();
        assert_eq!(set(&ws.workspace_id, Some(sel("files:link.txt", (0, 0), (0, 6), "secret"))), Ok(SelectionState::Held), "row 4b: a link out of the root is shared");
        assert_eq!(held(), Some(expect("link.txt", "secret", 1, 1)), "row 4b: a link out of the root publishes the file's text");
    }

    // 5. a failed publish answers publish_failed and leaves no file (a folder where the temporary file goes)
    assert!(set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"))).is_ok());
    assert!(held().is_some());
    // a folder at the temporary name `write_durable` writes first makes its removal fail
    std::fs::create_dir(folder.join("selection.json.tmp")).unwrap();
    assert_eq!(
        set(&ws.workspace_id, Some(sel("files:b.txt", (0, 0), (0, 3), "one"))),
        Err(selection_code::PUBLISH_FAILED),
        "row 5: the answer"
    );
    assert_eq!(held(), None, "row 5: a failed publish removes the earlier file");
    std::fs::remove_dir(folder.join("selection.json.tmp")).unwrap();

    // 6. a workspace that is no row, an empty id among them, writes nothing (a default row exists)
    assert!(set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"))).is_ok());
    let before = held();
    for id in ["", "ws-nope-9"] {
        assert_eq!(set(id, Some(sel("files:b.txt", (0, 0), (0, 3), "one"))), Err(selection_code::UNKNOWN_WORKSPACE), "row 6: {id:?}");
        assert_eq!(set(id, None), Err(selection_code::UNKNOWN_WORKSPACE), "row 6: a clear for {id:?}");
        assert_eq!(held(), before, "row 6: {id:?} writes nothing");
    }

    // 7. the read is bounded: a range near the start of a file past 2 MiB is accepted, one ending past the cap is not
    let cap = PREVIEW_BYTE_CAP;
    let long = "x".repeat(cap);
    std::fs::write(project.path().join("big.txt"), format!("head\n{long}\ntail\n")).unwrap();
    assert_eq!(set(&ws.workspace_id, Some(sel("files:big.txt", (0, 0), (0, 4), "head"))), Ok(SelectionState::Held));
    assert_eq!(held(), Some(expect("big.txt", "head", 1, 1)), "row 7: a range near the start of a large file");
    let exact = &long[..cap - 5];
    assert_eq!(set(&ws.workspace_id, Some(sel("files:big.txt", (1, 0), (1, (cap - 5) as u32), exact))), Ok(SelectionState::Held));
    assert_eq!(held(), Some(expect("big.txt", exact, 2, 2)), "row 7: a range ending exactly at the cap");
    let shown = &long[..cap - 4];
    assert_eq!(
        set(&ws.workspace_id, Some(sel("files:big.txt", (1, 0), (1, (cap - 4) as u32), shown))),
        Err(selection_code::NOT_FILE_TEXT),
        "row 7: a range ending past the cap"
    );
    assert_eq!(held(), None, "row 7: the refusal removes the file");

    // 8. a removal that fails answers publish_failed, whatever the request, and the old file stays (a non-empty folder
    // where the file goes cannot be removed or replaced, on Unix or Windows)
    std::fs::create_dir(&file).unwrap();
    std::fs::write(file.join("kept"), "x").unwrap();
    assert_eq!(set(&ws.workspace_id, None), Err(selection_code::PUBLISH_FAILED), "row 8: a clear whose removal fails");
    assert_eq!(
        set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"))),
        Err(selection_code::PUBLISH_FAILED),
        "row 8: an accepted selection whose publish and removal fail"
    );
    assert_eq!(
        set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alphX"))),
        Err(selection_code::PUBLISH_FAILED),
        "row 8: a refusal whose removal fails answers publish_failed, not its own code"
    );
    assert!(file.join("kept").exists(), "row 8: the old record stays");
    std::fs::remove_dir_all(&file).unwrap();

    // 9. a link to a device is no file: its open may wait or never end, so it is refused and the old file goes
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/dev/zero", project.path().join("zero.txt")).unwrap();
        assert!(set(&ws.workspace_id, Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"))).is_ok());
        assert_eq!(
            set(&ws.workspace_id, Some(sel("files:zero.txt", (0, 0), (0, 1), "\0"))),
            Err(selection_code::NOT_FILE_TEXT),
            "row 9: a link to /dev/zero"
        );
        assert_eq!(held(), None, "row 9: the refusal removes the file");
    }
}

#[tokio::test]
async fn the_replies_are_the_wire_frames() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    let mut row = Workspace::from_label("rep", project.path().to_path_buf(), false, "claude".into(), String::new(), String::new());
    row.workspace_id = "ws-rep-1".to_string();
    workspaces.insert(row);
    std::fs::create_dir_all(state_dir_for(state.path(), "ws-rep-1")).unwrap();
    std::fs::write(project.path().join("a.txt"), "alpha\n").unwrap();
    let ask = |root: Option<&Path>, workspace: &str, s: Option<SelectedText>| {
        let payload = serde_json::to_value(req(workspace, s)).unwrap();
        let workspaces = workspaces.clone();
        let root = root.map(Path::to_path_buf);
        async move { handle_selection_set(7, payload, root.as_deref(), &workspaces, true, 9_001_002).await.unwrap() }
    };
    let good = || Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"));
    let only = |out: HandlerOutput| {
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].0.id, out[0].0.op.as_str()), (7, op::SELECTION_SET));
        out[0].0.payload.clone()
    };
    let root = Some(state.path());
    assert_eq!(only(ask(root, "ws-rep-1", good()).await), serde_json::json!({"state": "held"}));
    assert_eq!(only(ask(root, "ws-rep-1", None).await), serde_json::json!({"state": "shared"}));
    let refused = Some(sel("files:a.txt", (0, 0), (0, 5), "alphX"));
    assert_eq!(
        only(ask(root, "ws-rep-1", refused).await),
        serde_json::json!({"error": "selection not shared: not_file_text", "code": "not_file_text"})
    );
    assert_eq!(only(ask(None, "ws-rep-1", good()).await)["code"], selection_code::PUBLISH_FAILED, "no state root");
    // a request the wire refuses at decode is a handler error and changes nothing
    let mut bad = serde_json::to_value(req("ws-rep-1", good())).unwrap();
    bad["selection"]["len"] = 0.into();
    assert_eq!(only(ask(root, "ws-rep-1", good()).await), serde_json::json!({"state": "held"}), "the file was published");
    assert!(handle_selection_set(7, bad, root, &workspaces, true, 9_001_002).await.is_err());
    assert!(state_dir_for(state.path(), "ws-rep-1").join(FILE_NAME).exists(), "a decode refusal changes nothing");
}

#[test]
fn the_reply_says_who_will_hear() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    let insert = |id: &str, agent: &str| {
        let mut row = Workspace::from_label(id, project.path().to_path_buf(), false, agent.into(), String::new(), String::new());
        row.workspace_id = id.to_string();
        std::fs::create_dir_all(state_dir_for(state.path(), id)).unwrap();
        workspaces.insert(row);
    };
    std::fs::write(project.path().join("a.txt"), "alpha\n").unwrap();
    let good = || Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"));
    let ask = |window: bool, id: &str, s: Option<SelectedText>| set_selection(Some(state.path()), &workspaces, window, 9_001_003, &req(id, s));
    let file = |id: &str| state_dir_for(state.path(), id).join(FILE_NAME);

    // 1. a connection that is not a window's is refused before any row work, and a file a window set stays
    insert("ws-claude", "claude");
    assert!(ask(true, "ws-claude", good()).is_ok());
    for (name, s) in [("a set", good()), ("a clear", None)] {
        assert_eq!(ask(false, "ws-claude", s), Err(selection_code::NOT_A_WINDOW), "row 1: {name}");
        assert!(file("ws-claude").exists(), "row 1: {name} changes nothing");
    }
    assert_eq!(ask(false, "ws-nope", good()), Err(selection_code::NOT_A_WINDOW), "row 1: checked before the row");
    assert_eq!(set_selection(None, &workspaces, false, 9_001_003, &req("ws-claude", good())), Err(selection_code::NOT_A_WINDOW), "row 1: checked before the state root");

    // 2. a kind that takes none is refused and loses its file; a clear answers shared for any row
    for kind in ["claude", "codex"] {
        insert(&format!("ws-{kind}-k"), kind);
        assert!(ask(true, &format!("ws-{kind}-k"), good()).is_ok(), "row 2: {kind} takes one");
    }
    for kind in ["none", "gemini", ""] {
        let id = format!("ws-{kind}-k");
        insert(&id, kind);
        std::fs::write(file(&id), "old").unwrap();
        assert_eq!(ask(true, &id, good()), Err(selection_code::NO_AGENT), "row 2: {kind:?} takes none");
        assert!(!file(&id).exists(), "row 2: {kind:?} refusal removes the file");
        assert_eq!(ask(true, &id, None), Ok(SelectionState::Shared), "row 2: a clear for {kind:?}");
    }

    // 3. an accepted selection is shared when the marker exists and held when it does not
    assert_eq!(ask(true, "ws-claude", good()), Ok(SelectionState::Held), "row 3: no marker");
    assert_eq!(ask(true, "ws-codex-k", good()), Ok(SelectionState::Held), "row 3: codex with no marker");
    std::fs::write(state_dir_for(state.path(), "ws-claude").join(MARKER_NAME), "").unwrap();
    assert_eq!(ask(true, "ws-claude", good()), Ok(SelectionState::Shared), "row 3: marker");
    std::fs::remove_file(state_dir_for(state.path(), "ws-claude").join(MARKER_NAME)).unwrap();
    assert_eq!(ask(true, "ws-claude", good()), Ok(SelectionState::Held), "row 3: marker removed");
}

#[test]
fn a_setters_end_or_the_start_up_walk_withdraws_only_stale_selections() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new();
    for id in ["ws-wd-1", "ws-wd-2", "ws-wd-3"] {
        let mut row = Workspace::from_label(id, project.path().to_path_buf(), false, "claude".into(), String::new(), String::new());
        row.workspace_id = id.to_string();
        std::fs::create_dir_all(state_dir_for(state.path(), id)).unwrap();
        workspaces.insert(row);
    }
    std::fs::write(project.path().join("a.txt"), "alpha\n").unwrap();
    let good = || Some(sel("files:a.txt", (0, 0), (0, 5), "alpha"));
    let file = |id: &str| state_dir_for(state.path(), id).join(FILE_NAME);
    let ask = |window: bool, serial: u64, id: &str, s: Option<SelectedText>| {
        set_selection(Some(state.path()), &workspaces, window, serial, &req(id, s))
    };
    let (a, b) = (9_002_001, 9_002_002);

    // 1. a publish replaces the setter; a clear, a refusal and a failed publish drop it; not_a_window changes nothing
    assert!(ask(true, a, "ws-wd-1", good()).is_ok());
    assert!(ask(true, b, "ws-wd-1", good()).is_ok());
    forget(a);
    assert!(file("ws-wd-1").exists(), "row 1: a publish replaces the setter, so the first connection's end leaves the file");
    assert!(ask(false, a, "ws-wd-1", good()).is_err());
    forget(a);
    assert!(file("ws-wd-1").exists(), "row 1: not_a_window records no setter");
    for (name, s) in [("a clear", None), ("a refusal", Some(sel("files:a.txt", (0, 0), (0, 5), "alphX")))] {
        assert!(ask(true, a, "ws-wd-1", good()).is_ok());
        let _ = ask(true, b, "ws-wd-1", s);
        std::fs::write(file("ws-wd-1"), "later").unwrap();
        forget(a);
        assert!(file("ws-wd-1").exists(), "row 1: {name} drops the entry, so a later end removes nothing");
        forget(b);
    }
    assert!(ask(true, a, "ws-wd-1", good()).is_ok());
    std::fs::create_dir(folder_tmp(&file("ws-wd-1"))).unwrap();
    assert!(ask(true, b, "ws-wd-1", good()).is_err());
    std::fs::remove_dir(folder_tmp(&file("ws-wd-1"))).unwrap();
    std::fs::write(file("ws-wd-1"), "later").unwrap();
    forget(a);
    assert!(file("ws-wd-1").exists(), "row 1: a failed publish drops the entry");
    forget(b);

    // a removal that fails keeps the setter's entry, so its end retries
    assert!(ask(true, a, "ws-wd-1", good()).is_ok());
    std::fs::remove_file(file("ws-wd-1")).unwrap();
    std::fs::create_dir(file("ws-wd-1")).unwrap();
    std::fs::write(file("ws-wd-1").join("kept"), "x").unwrap();
    assert_eq!(ask(true, b, "ws-wd-1", None), Err(selection_code::PUBLISH_FAILED));
    std::fs::remove_dir_all(file("ws-wd-1")).unwrap();
    std::fs::write(file("ws-wd-1"), "later").unwrap();
    forget(a);
    assert!(!file("ws-wd-1").exists(), "row 1: a failed removal keeps the entry, so its setter's end removes the file");

    // 2. an end removes every file its serial set and nothing another serial set
    assert!(ask(true, a, "ws-wd-2", good()).is_ok());
    assert!(ask(true, a, "ws-wd-3", good()).is_ok());
    assert!(ask(true, b, "ws-wd-3", good()).is_ok());
    forget(a);
    assert!(!file("ws-wd-2").exists(), "row 2: the end removes the file its serial set");
    assert!(file("ws-wd-3").exists(), "row 2: the end leaves a file another serial set");
    forget(b);
    assert!(!file("ws-wd-3").exists(), "row 2: the other serial's end removes it");

    // 3. the roster's guard ends the connection: dropping it removes what its serial set
    let clients = crate::clients::Clients::new();
    let guard = clients.register("c", "0.6.6", 1, "fe".into(), Some("host-a".into()), Some("w".into()), Some("fe@host-a".to_string()));
    assert!(ask(true, guard.serial(), "ws-wd-2", good()).is_ok());
    assert!(file("ws-wd-2").exists());
    let other = crate::clients::Clients::new().register("c", "0.6.6", 1, "fe".into(), None, None, None);
    assert_ne!(other.serial(), guard.serial(), "row 3: two rosters never hand out one serial");
    drop(guard);
    assert!(!file("ws-wd-2").exists(), "row 3: a guard's end removes the file its connection set");

    // 4. the walk removes a file no live connection of this process set, and keeps one a connection set
    std::fs::write(file("ws-wd-1"), "from an earlier process").unwrap();
    assert!(clear_unowned(state.path(), "ws-wd-1"));
    assert!(!file("ws-wd-1").exists(), "row 4: a file from an earlier process is removed");
    assert!(!clear_unowned(state.path(), "ws-wd-1"), "row 4: a row with no file does no removal");
    assert!(ask(true, a, "ws-wd-2", good()).is_ok());
    clear_unowned(state.path(), "ws-wd-2");
    assert!(file("ws-wd-2").exists(), "row 4: a share made before the walk survives it");
    forget(a);
}

/// The temporary file `write_durable` creates first, so a folder there makes a publish fail.
fn folder_tmp(file: &Path) -> PathBuf {
    file.with_extension("json.tmp")
}
