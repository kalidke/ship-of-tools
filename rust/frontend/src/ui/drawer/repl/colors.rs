//! The REPL drawer's colors, in one place. The drawer is the window's own view of a Julia session, not a terminal
//! running Julia's REPL, so it picks every color itself; where Julia's REPL has a counterpart the color is Julia's,
//! as the 16 named terminal colors (`ratatui::style::Color`'s named variants), which `render::cells` maps to RGB
//! as it does for the Terminal drawer and the agent pane. Sources: Julia 1.13 `Base/client.jl` (the `default_color_*`
//! constants), `REPL/src/REPL.jl` (the `LineEditREPL` constructor's prompt colors) and `REPL/src/LineEdit.jl`
//! (`write_prompt` prints the prompt text in bold). No use site holds a literal color.

use ratatui::style::{Color, Modifier, Style};

/// `julia> `: `:green` (REPL.jl `LineEditREPL`, `Base.text_colors[:green]`).
const JULIA_PROMPT: Color = Color::Green;
/// `pkg> `: `:blue` (REPL.jl, `pkg_color`).
const PKG_PROMPT: Color = Color::Blue;
/// `ERROR: `: `:light_red`, bold (client.jl `default_color_error`; `display_error`'s `printstyled(bold=true)`).
const ERROR_LABEL: Color = Color::LightRed;
/// A stack frame's `@`, module, file and line: `:light_black`, which is ANSI 8 (`Base.show_backtrace`).
const TRACE_GRAY: Color = Color::DarkGray;

// What Julia's REPL has no counterpart for: the drawer's own words and marks.
/// The live prompt while the drawer does not have focus (the window's attention cue).
const UNFOCUSED_PROMPT: Color = Color::DarkGray;
/// The marker above code another session ran.
const ORIGIN: Color = Color::LightMagenta;
/// The line announcing an interactive figure.
const BROWSER_LINK: Color = Color::LightBlue;
/// The caption standing for a figure that is not drawn.
const IMAGE_NOTE: Color = Color::LightMagenta;
/// The note while Julia is starting.
const STARTING: Color = Color::Yellow;
/// The `(N ms)` line.
const ELAPSED: Color = Color::DarkGray;

fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn dim(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::DIM)
}

/// Stdout, stderr, a shown value, the typed code and an error's message: Julia prints them in the terminal's own
/// color (`default_color_input` and `default_color_answer` are `:normal`).
pub(in crate::ui) fn plain() -> Style {
    Style::default()
}

/// The prompt's text in its mode's color, bold; the unfocused live prompt keeps the weight and loses the color.
pub(in crate::ui) fn prompt(pkg_mode: bool, focused: bool) -> Style {
    match (focused, pkg_mode) {
        (false, _) => bold(UNFOCUSED_PROMPT),
        (true, true) => bold(PKG_PROMPT),
        (true, false) => bold(JULIA_PROMPT),
    }
}

/// The live prompt's cursor block, in the prompt's color.
pub(in crate::ui) fn cursor(pkg_mode: bool) -> Style {
    Style::default().fg(if pkg_mode { PKG_PROMPT } else { JULIA_PROMPT })
}

pub(in crate::ui) fn error_label() -> Style {
    bold(ERROR_LABEL)
}

/// The words around a stack frame's function and location.
pub(in crate::ui) fn trace_gray() -> Style {
    Style::default().fg(TRACE_GRAY)
}

/// A stack frame's function name: bold in the default color.
pub(in crate::ui) fn trace_function() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub(in crate::ui) fn origin() -> Style {
    bold(ORIGIN)
}

pub(in crate::ui) fn browser_link() -> Style {
    Style::default().fg(BROWSER_LINK)
}

pub(in crate::ui) fn image_note() -> Style {
    dim(IMAGE_NOTE)
}

pub(in crate::ui) fn starting() -> Style {
    dim(STARTING)
}

pub(in crate::ui) fn running() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub(in crate::ui) fn elapsed() -> Style {
    dim(ELAPSED)
}

#[cfg(test)]
#[path = "colors_tests.rs"]
mod tests;
