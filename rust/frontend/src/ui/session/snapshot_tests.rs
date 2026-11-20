use super::*;
use crate::ui::preview::image::ink::InkLayer;
use sot_protocol::ops::InkBase;

#[test]
fn same_slug_across_hosts_produces_distinct_ws_keys_and_snapshot_slots() {
    // ADR 0042 L2a codex review, item B: the row snapshot map, now ParkedViews (and
    // every sibling map) used to key by bare slug, so switching from
    // "sot" on host alpha to "sot" on host beta was the SAME map key
    // — restore_workspace_ui found alpha's stashed snapshot and
    // repainted it under beta. WsKey = (host, slug) makes the two
    // entries distinct slots, so a same-slug cross-host switch can
    // only ever hit its own host's entry.
    let key_alpha: WsKey = ("alpha".to_string(), "sot".to_string());
    let key_beta: WsKey = ("beta".to_string(), "sot".to_string());
    assert_ne!(
        key_alpha, key_beta,
        "same slug on two different hosts must not collide"
    );

    let mut snaps: HashMap<WsKey, &'static str> = HashMap::new();
    snaps.insert(key_alpha.clone(), "alpha's snapshot");
    snaps.insert(key_beta.clone(), "beta's snapshot");
    assert_eq!(snaps.get(&key_alpha), Some(&"alpha's snapshot"));
    assert_eq!(
        snaps.get(&key_beta),
        Some(&"beta's snapshot"),
        "beta's own snapshot must survive alpha's insert under the same slug"
    );
}

fn parked_view(page: Option<(u32, u32)>) -> WorkspaceUiSnapshot {
    WorkspaceUiSnapshot {
        mode: Mode::Files,
        bl_pane_target: None,
        preview_node_id_fired: None,
        preview_refused: false,
        pinned_preview_node_id: None,
        source: Default::default(),
        figure_failed: Default::default(),
        concept: None,
        edit_state: None,
        ink: Default::default(),
        drawer: RowDrawer::default(),
        page,
    }
}

#[test]
fn a_restored_row_keeps_its_pdf_page_and_a_row_without_pages_gets_none() {
    // A row's parked view carries the page of the bytes it shows; the restore applies it (as `restore_workspace_ui`
    // does, before the repaint) so the repaint picks the paged sampler and the page keys stay admitted. Before, the
    // restore left `PageView` as the departed row had it: a paged row came back unpaged (page keys dead, Nearest
    // sampler) and an unpaged row inherited the departed row's page.
    let (paged, plain): (WsKey, WsKey) = (("h".into(), "a".into()), ("h".into(), "b".into()));
    let mut parked = ParkedViews::default();
    let mut pages = crate::ui::preview::image::pages::PageView::default();
    pages.set_page(Some((3, 5)));
    parked.park(paged.clone(), parked_view(pages.page()));
    pages.set_page(None);
    parked.park(plain.clone(), parked_view(pages.page()));
    // Back to the paged row, then to the plain row (which must not keep the paged row's page).
    pages.set_page(parked.view(&paged).unwrap().page);
    assert_eq!(pages.page(), Some((3, 5)));
    assert!(pages.page().is_some_and(|(_, count)| count > 1), "the help context admits preview.page_next");
    pages.set_page(parked.view(&plain).unwrap().page);
    assert_eq!(pages.page(), None);
}

fn drawn(layer: &mut InkLayer, base: &InkBase, x: f32) {
    let r = crate::ui::render::quad::ScreenRect { x: 0.0, y: 0.0, w: 400.0, h: 200.0 };
    layer.set_on(true);
    layer.press(Some(base), (x, 100.0), &r, &r, (400, 200));
    layer.release();
}

#[test]
fn a_rows_drawing_comes_back_with_its_view() {
    // Two rows show the same relative path and each draws: each comes back with its own strokes and mode, and a row
    // destroyed while parked leaves nothing for a same-slug successor.
    let base = InkBase::Figure { node_id: "files:plots/fit.png".into() };
    let (a, b): (WsKey, WsKey) = (("h".into(), "a".into()), ("h".into(), "b".into()));
    let mut parked = ParkedViews::default();
    let mut layer = InkLayer::default();
    drawn(&mut layer, &base, 100.0);
    parked.park(a.clone(), WorkspaceUiSnapshot { ink: layer.park(), ..parked_view(None) });
    layer.restore(Default::default());
    assert!(layer.strokes().is_empty() && !layer.is_on(), "a row entered with no parked view starts clean");
    drawn(&mut layer, &base, 50.0);
    drawn(&mut layer, &base, 300.0);
    layer.set_on(false);
    parked.park(b.clone(), WorkspaceUiSnapshot { ink: layer.park(), ..parked_view(None) });
    layer.restore(parked.view(&a).map(|v| v.ink.clone()).unwrap_or_default());
    assert_eq!((layer.strokes().len(), layer.is_on(), layer.base()), (1, true, Some(&base)));
    layer.restore(parked.view(&b).map(|v| v.ink.clone()).unwrap_or_default());
    assert_eq!((layer.strokes().len(), layer.is_on()), (2, false));
    parked.forget(&a);
    layer.restore(parked.view(&a).map(|v| v.ink.clone()).unwrap_or_default());
    assert!(layer.strokes().is_empty() && !layer.is_on() && layer.base().is_none(), "a destroyed row's drawing is gone");
}

/// Every way of changing the shown row saves the row left and restores the row entered, because only one function
/// selects a row: `retarget_workspace`, which snapshots first (`snapshot_current_workspace_ui`, the drawer and its height
/// with it), and the capture harness's startup seed, which has no row to leave. A second caller of `SelectedRow::select`
/// is a way of changing the row that parks nothing.
#[test]
fn only_the_one_switch_selects_a_row() {
    let mut found = Vec::new();
    let mut folders = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                folders.push(path);
            } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let calls = text.lines().filter(|l| !l.trim_start().starts_with("//") && l.contains("selected.select(")).count();
                if calls > 0 { found.push((path.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display().to_string(), calls)); }
            }
        }
    }
    found.sort();
    assert_eq!(found, [("src/ui/init/mod.rs".to_string(), 1), ("src/ui/session/switch.rs".to_string(), 1)],
        "a row is selected outside `retarget_workspace`, so a switch there parks nothing");
}
