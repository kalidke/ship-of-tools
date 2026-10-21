use super::*;

#[test]
fn initial_mode_resumes_the_persisted_mode_unless_the_cli_names_one() {
    assert_eq!(initial_mode(None, Some("sessions"), false), Mode::Sessions);
    assert_eq!(initial_mode(Some("hosts"), Some("modules"), false), Mode::Hosts);
    assert_eq!(initial_mode(Some("files"), Some("sessions"), false), Mode::Files);
    assert_eq!(initial_mode(None, None, false), Mode::Files);
    assert_eq!(initial_mode(None, Some("bogus"), false), Mode::Files);
    assert_eq!(initial_mode(None, Some("sessions"), true), Mode::Files);
    assert_eq!(initial_mode(Some("hosts"), Some("modules"), true), Mode::Hosts);
}

/// The mode names the window persists and `--start-mode` reads: each
/// `label()` round-trips through `initial_mode`.
#[test]
fn mode_label_names_each_mode() {
    for (m, name) in [
        (Mode::Files, "files"),
        (Mode::Modules, "modules"),
        (Mode::Sessions, "sessions"),
        (Mode::Hosts, "hosts"),
    ] {
        assert_eq!(m.label(), name);
        assert_eq!(initial_mode(Some(m.label()), None, false), m);
    }
    // Names are matched exactly: case and aliases are not folded.
    assert_eq!(initial_mode(Some("Hosts"), None, false), Mode::Files);
    assert_eq!(initial_mode(Some("SESSIONS"), None, false), Mode::Files);
    assert_eq!(initial_mode(Some("module"), None, false), Mode::Files);
}

/// Pins the workspace-identity equivalences the deleted
/// `tree_matches_active_ws` test pinned, now expressed as tree-KEY
/// equality — the comparison every install route makes. Same semantics:
/// same named ws → same slot; both daemon-default → same slot;
/// default-vs-named (either direction) and named-vs-other-named →
/// different slots.
#[test]
fn tree_key_normalization_covers_default_and_mismatch() {
    // No default slug known (boot): only None means the default ws.
    // ADR 0042 L2a: mode_scope takes a WsKey now -- pin a fixed host so
    // this test still isolates the slug-collapse behavior it's for.
    let k = |ws: Option<&str>| -> TreeKey {
        let wk: WsKey = ("h".to_string(), ws_key_of(ws, None));
        (Mode::Files, mode_scope(Mode::Files, &wk))
    };
    assert_eq!(k(Some("demo-a")), k(Some("demo-a")));
    assert_eq!(k(None), k(None));
    // The original desync: demo-b reply while demo-a is active
    // → different key → parks in its own slot, can't clobber.
    assert_ne!(k(Some("demo-b")), k(Some("demo-a")));
    assert_ne!(k(None), k(Some("demo-a")));
    assert_ne!(k(Some("demo-a")), k(None));
    // The wire's None must normalize to the SAME literal
    // `current_workspace_key` uses for the default workspace.
    assert_eq!(ws_key_of(None, None), "<default>");
    // DEFAULT-SLUG ALIASING: the daemon default is addressable two ways
    // — `None` (startup) and `Some(default_slug)` (cycling /
    // Sessions-Enter). Both MUST collapse to the same key, or one
    // physical workspace splits into two slots (cursor reset +
    // duplicate root on every cycle to the default).
    assert_eq!(ws_key_of(None, Some("mypkg")), "<default>");
    assert_eq!(ws_key_of(Some("mypkg"), Some("mypkg")), "<default>");
    assert_eq!(
        ws_key_of(Some("mypkg"), Some("mypkg")),
        ws_key_of(None, Some("mypkg"))
    );
    // A NON-default slug is untouched by the collapse.
    assert_eq!(ws_key_of(Some("mypkg"), Some("other")), "mypkg");
    // Mode is half the key: same ws, different mode → different slot
    // (the set_flat hole — a Modules scan can never key to a Files view).
    let wk_a: WsKey = ("h".to_string(), "a".to_string());
    assert_ne!(
        (Mode::Files, mode_scope(Mode::Files, &wk_a)),
        (Mode::Modules, mode_scope(Mode::Modules, &wk_a))
    );
    // Sessions/Hosts are machine-level: Global, workspace-independent
    // (and host-independent — the tree isn't per-host in scope).
    let wk_ws_a: WsKey = ("h".to_string(), "wsA".to_string());
    let wk_ws_b: WsKey = ("h".to_string(), "wsB".to_string());
    assert_eq!(
        mode_scope(Mode::Sessions, &wk_ws_a),
        mode_scope(Mode::Sessions, &wk_ws_b)
    );
    let wk_any: WsKey = ("h".to_string(), "anything".to_string());
    assert_eq!(mode_scope(Mode::Hosts, &wk_any), TreeScope::Global);
}

/// TreeStore slot isolation + round-trip: an install into one key's slot
/// can't touch another key's; stash/take round-trips rows, cursor, and
/// scroll (per-mode cursor persistence rides on exactly this); a missing
/// key takes as None (a first visit gets an empty view, never another
/// slot's rows — the Modules→Files restore-validity hole).
#[test]
fn tree_store_isolates_slots_and_round_trips() {
    let mut store = TreeStore::new();
    let key_files_a: TreeKey = (
        Mode::Files,
        TreeScope::Workspace(("h".to_string(), "wsA".to_string())),
    );
    let key_files_b: TreeKey = (
        Mode::Files,
        TreeScope::Workspace(("h".to_string(), "wsB".to_string())),
    );
    let key_mods_a: TreeKey = (
        Mode::Modules,
        TreeScope::Workspace(("h".to_string(), "wsA".to_string())),
    );
    let key_sessions: TreeKey = (Mode::Sessions, TreeScope::Global);

    // Install into (Files, wsA) — as the routed non-active branch does.
    store.slot_mut(key_files_a.clone()).view.set_root(
        node("files:", "a", true),
        vec![node("files:x.jl", "x.jl", false)],
    );
    // (a) another workspace's Files slot is untouched…
    assert!(store.take(&key_files_b).is_none());
    // (b) …and so is the SAME workspace's Modules slot (mode is in the key).
    store
        .slot_mut(key_mods_a.clone())
        .view
        .set_flat(vec![TreeRow {
            node: node("modules:M", "M", false),
            depth: 0,
            expanded: false,
        }]);
    // (e) the Global Sessions slot is independent of any workspace slot.
    store
        .slot_mut(key_sessions.clone())
        .view
        .set_root(node("sessions:", "s", true), Vec::new());

    // Round-trip preserves rows + cursor + scroll (per-mode cursor
    // persistence is exactly this).
    let mut slot = store.take(&key_files_a).expect("wsA Files slot present");
    assert_eq!(slot.view.rows.len(), 2);
    slot.view.selected = 1;
    slot.scroll = 7;
    store.stash(key_files_a.clone(), slot);
    let again = store.take(&key_files_a).expect("round-trip");
    assert_eq!(again.view.rows[1].node.id, "files:x.jl");
    assert_eq!(again.view.selected, 1);
    assert_eq!(again.scroll, 7);

    // The Modules slot still holds ONLY modules rows; taking the (now
    // consumed) Files key yields None — a Files visit after this can
    // never be handed modules: rows.
    assert!(store.take(&key_files_a).is_none());
    let mods = store.take(&key_mods_a).expect("wsA Modules slot present");
    assert!(mods
        .view
        .rows
        .iter()
        .all(|r| r.node.id.starts_with("modules:")));
    assert!(store.take(&key_sessions).is_some());
}
