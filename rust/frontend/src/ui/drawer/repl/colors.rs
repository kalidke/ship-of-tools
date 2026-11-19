//! The REPL drawer's colors, in one place. The drawer is the window's own view of a Julia session, not a terminal
//! running Julia's REPL, so it picks every color itself; where Julia's REPL has a counterpart the color is Julia's,
//! as the 16 named terminal colors (`ratatui::style::Color`'s named variants), which `render::cells` maps to RGB
//! as it does for the Terminal drawer and the agent pane. Sources: Julia 1.13 `Base/client.jl` (the `default_color_*`
//! constants), `REPL/src/REPL.jl` (the `LineEditREPL` constructor's prompt colors), `REPL/src/LineEdit.jl`
//! (`write_prompt` prints the prompt text in bold) and `Base/stacktraces.jl` (`show_spec_linfo`, which frames get a
//! bold name). No use site holds a literal color. The one place the drawer departs from Julia's REPL on purpose is
//! `stderr`, below.

use ratatui::style::{Color, Modifier, Style};

/// `julia> `: `:green` (REPL.jl `LineEditREPL`, `Base.text_colors[:green]`).
const JULIA_PROMPT: Color = Color::Green;
/// `pkg> `: `:blue` (the Pkg REPL mode's `Base.text_colors[:blue]`; REPL.jl's `pkg_color` is the same color).
const PKG_PROMPT: Color = Color::Blue;
/// `ERROR: `: `:light_red`, bold (client.jl `default_color_error`; `display_error`'s `printstyled(bold=true)`).
const ERROR_LABEL: Color = Color::LightRed;
/// A stack frame's `@`, file and line, and the `Base` and `Core` modules: `:light_black`, which is ANSI 8
/// (`Base.show_backtrace`).
const TRACE_GRAY: Color = Color::DarkGray;

// What Julia's REPL has no counterpart for: the drawer's own words and marks.
/// Text a program wrote to stderr. Julia's REPL prints it in the terminal's own color, so a log read back later
/// cannot tell it from stdout; the drawer keeps the difference with a gray (ANSI 7, 170 on the pane's near-black
/// surface, contrast 8.5 against 12.5 for the default text), which Julia's REPL uses for nothing, so it reads as
/// neither an error (`ERROR: ` is bold light red) nor a warning (yellow).
const STDERR: Color = Color::Gray;
/// A note the window wrote into the log itself (the link is back), told from the session's output.
const NOTICE: Color = Color::LightMagenta;
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

/// Stdout, a shown value, the typed code and an error's message: Julia prints them in the terminal's own color
/// (`default_color_input` and `default_color_answer` are `:normal`).
pub(in crate::ui) fn plain() -> Style {
    Style::default()
}

/// Text a program wrote to stderr: the drawer's one deliberate departure from Julia's REPL (see `STDERR`).
pub(in crate::ui) fn stderr() -> Style {
    Style::default().fg(STDERR)
}

pub(in crate::ui) fn notice() -> Style {
    Style::default().fg(NOTICE)
}

/// The prompt echoed in the scrollback: always in its mode's color.
pub(in crate::ui) fn scrollback_prompt(pkg_mode: bool) -> Style {
    prompt(pkg_mode, true)
}

/// The live prompt's text in its mode's color, bold; unfocused, it keeps the weight and loses the color.
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

/// The drawer's own punctuation around a stack frame's function and location, and the frame's gray.
pub(in crate::ui) fn trace_gray() -> Style {
    Style::default().fg(TRACE_GRAY)
}

/// A stack frame's function name: bold in the default color, except what Julia prints plain, `top-level scope`
/// (`Base/stacktraces.jl` `show_spec_linfo`) and a frame with no name (Julia prints `ip:0x...`). The child sends the
/// name alone, so a frame Julia prints as `top-level scope` from its code info under another name is bold here.
pub(in crate::ui) fn trace_function(name: &str) -> Style {
    if name.is_empty() || name == "top-level scope" {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
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
