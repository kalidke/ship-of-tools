use super::chord_types_text;

/// Ctrl+Q reaches the editor (it has the keys first), and winit can name its key by the bare letter.
#[test]
fn a_ctrl_chord_that_is_no_editor_action_types_nothing() {
    assert!(chord_types_text(false, false, false), "a plain letter types");
    assert!(chord_types_text(false, false, false) && chord_types_text(false, true, false), "Alt+letter keeps its text");
    assert!(!chord_types_text(true, false, false), "Ctrl+Q does not type a q");
    assert!(chord_types_text(true, true, false), "AltGr (Ctrl+Alt on Windows) types");
    assert_eq!(chord_types_text(false, false, true), !cfg!(target_os = "macos"), "Cmd chords on macOS do not");
}
