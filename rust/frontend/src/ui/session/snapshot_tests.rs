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
        pinned_preview_node_id: None,
        source: Default::default(),
        figure_failed: Default::default(),
        concept: None,
        edit_state: None,
        ink: Default::default(),
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
    layer.restore(None);
    assert!(layer.strokes().is_empty() && !layer.is_on(), "a row entered with no parked view starts clean");
    drawn(&mut layer, &base, 50.0);
    drawn(&mut layer, &base, 300.0);
    parked.park(b.clone(), WorkspaceUiSnapshot { ink: layer.park(), ..parked_view(None) });
    layer.restore(parked.view(&a).map(|v| v.ink.clone()));
    assert_eq!((layer.strokes().len(), layer.is_on(), layer.base()), (1, true, Some(&base)));
    layer.restore(parked.view(&b).map(|v| v.ink.clone()));
    assert_eq!((layer.strokes().len(), layer.is_on()), (2, true));
    parked.forget(&a);
    layer.restore(parked.view(&a).map(|v| v.ink.clone()));
    assert!(layer.strokes().is_empty() && !layer.is_on() && layer.base().is_none(), "a destroyed row's drawing is gone");
}
