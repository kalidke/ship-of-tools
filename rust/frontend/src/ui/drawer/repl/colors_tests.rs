use super::*;
use crate::ui::drawer::terminal::vt::vt100_color_to_ratatui;

/// The 16 named colors Julia's `Base.text_colors` has, as the SGR color index a terminal reads them from
/// (`\e[32m` is index 2, `\e[90m` is index 8, `\e[91m` is index 9).
fn ansi_index(julia_name: &str) -> u8 {
    match julia_name {
        "red" => 1,
        "green" => 2,
        "blue" => 4,
        "light_black" => 8,
        "light_red" => 9,
        other => panic!("not in the table: {other}"),
    }
}

#[test]
fn every_table_color_is_the_terminal_drawers_color_for_julias_name() {
    // The Terminal drawer and the agent pane map an SGR index to a named `Color` (`vt100_color_to_ratatui`) and
    // `render::cells` maps that to RGB; the REPL drawer names the same `Color`, so Julia's `:green` is the one green.
    let named = |name| vt100_color_to_ratatui(vt100::Color::Idx(ansi_index(name)));
    assert_eq!(JULIA_PROMPT, named("green"));
    assert_eq!(PKG_PROMPT, named("blue"));
    assert_eq!(ERROR_LABEL, named("light_red"));
    assert_eq!(TRACE_GRAY, named("light_black"));
}

#[test]
fn the_prompt_is_bold_and_only_the_cursor_is_not() {
    for (pkg, color) in [(false, Color::Green), (true, Color::Blue)] {
        let style = prompt(pkg, true);
        assert_eq!((style.fg, style.add_modifier.contains(Modifier::BOLD)), (Some(color), true));
        assert_eq!(cursor(pkg).fg, Some(color));
        assert!(!cursor(pkg).add_modifier.contains(Modifier::BOLD));
        // Unfocused: the color is the attention cue's, the weight stays the prompt's.
        let unfocused = prompt(pkg, false);
        assert_eq!((unfocused.fg, unfocused.add_modifier.contains(Modifier::BOLD)), (Some(UNFOCUSED_PROMPT), true));
    }
}
