//! Display-free behavioral proofs of the paths used by the native drivers.

use super::*;

#[test]
#[cfg(feature = "test-pane-timing")]
fn pane_child_layout_has_agent_at_any_monitor_shape() {
    use crate::ui::persist::settings::Slot;
    let settings = super::super::native_pane_tests::pane_child_settings();
    for aspect in [4.0 / 3.0, 0.75, 1.5, 1.6] {
        assert!(settings.resolve_preset(aspect).columns.contains(&Slot::Llm),
            "pane child has no agent column at aspect {aspect}");
    }
    assert!(!Settings::default().resolve_preset(0.75).columns.contains(&Slot::Llm));
}
