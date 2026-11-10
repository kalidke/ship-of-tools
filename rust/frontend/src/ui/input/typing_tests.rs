use super::chord_types_text;

/// The window's one typing rule, which the editor and the chat draft both call: winit can name a key by its bare letter
/// under Ctrl, so a Ctrl chord bound to nothing must not type it, while AltGr must.
#[test]
fn a_chord_types_only_where_the_window_says_it_types_text() {
    assert!(chord_types_text(false, false, false), "a plain letter types");
    assert!(chord_types_text(false, false, false) && chord_types_text(false, true, false), "Alt+letter keeps its text");
    assert!(!chord_types_text(true, false, false), "Ctrl+Q does not type a q");
    assert!(chord_types_text(true, true, false), "AltGr (Ctrl+Alt on Windows) types");
    assert_eq!(chord_types_text(false, false, true), !cfg!(target_os = "macos"), "Cmd chords on macOS do not");
}
