//! Clipboard dispatch without depending on a desktop clipboard service.

use super::copy_selection_status;
use crate::ui::preview::{markdown::{MarkdownPreview, highlight::HighlightService}, shaped::ShapedPreview};
use crate::ui::render::{quad::ScreenRect, text::EXTRA_TOP_PAD_PX};

#[test]
fn preview_copy_writes_selected_text_and_reports_success_or_failure() {
    let md = MarkdownPreview::new_plain(&mut glyphon::FontSystem::new(), "alpha\nbravo", 200.0, 1.0);
    let mut p = ShapedPreview::new(md, HighlightService::new().unwrap(),
        ScreenRect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 });
    assert!(copy_selection_status(&p, |_| panic!("no selection must not open the clipboard")).is_none());
    let start = p.hit((0.0, EXTRA_TOP_PAD_PX + 5.0), true).unwrap();
    p.press(Some(start));
    p.extend_drag((199.0, EXTRA_TOP_PAD_PX + 25.0));
    p.release();
    let mut copied = None;
    assert_eq!(copy_selection_status(&p, |text| { copied = Some(text); Ok(()) }).as_deref(),
        Some("copied preview selection"));
    assert_eq!(copied.as_deref(), Some("alpha\nbravo"));
    assert_eq!(copy_selection_status(&p, |_| Err("clipboard unavailable".into())).as_deref(),
        Some("copy failed · clipboard unavailable"));
    assert_eq!(p.selected_text().as_deref(), Some("alpha\nbravo"), "a copy leaves the selection available");
}
