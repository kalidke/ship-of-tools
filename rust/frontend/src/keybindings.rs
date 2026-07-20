// keybindings.rs — user-configurable keybindings for the frontend.
//
// Phase-1 surface intentionally tiny: a handful of named actions, each
// mapped to one or more chord strings (e.g. "Alt+=" or "Ctrl+ArrowRight").
// Defaults live in `KeyBindings::defaults()`. A `keybindings.toml` file
// is read at startup and overrides the defaults action-by-action; missing
// actions in the file fall through to defaults so the user only writes
// what they want to change.
//
// File discovery, in priority order:
//   1. $SOT_KEYBINDINGS  — explicit override path
//   2. <repo-root>/.sot/keybindings.toml  — project-level
//   3. $HOME/.config/sot/keybindings.toml  — user-level
//
// File format is a single `[keys]` table mapping action → chord (or list
// of chords). The parser is dirt-simple — only `key = "value"` and
// `key = ["v1", "v2"]` lines are recognised; comments start with `#`;
// other lines are ignored. Real TOML is overkill for this surface.

use std::fs;
use std::path::{Path, PathBuf};

use winit::keyboard::{Key, NamedKey};

/// Named actions the chrome can bind to. Add a variant here, give it a
/// default chord in `KeyBindings::defaults()`, and the keybindings file
/// can override it. Existing hardcoded keybinds (Ctrl+Arrow focus move,
/// f/m mode switch, q/Esc quit) will migrate here incrementally — for
/// now the file can only customise the actions listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Maximise the focused pane to fill the window.
    MaximizePane,
    /// Restore the 2×2 quadrant layout.
    RestoreLayout,
    /// PNG preview pane: zoom the canvas in.
    PreviewPngZoomIn,
    /// PNG preview pane: zoom the canvas out (clamped at fit-to-pane).
    PreviewPngZoomOut,
    /// PNG preview pane: reset zoom + pan to fit-to-pane, centred.
    PreviewPngReset,
    /// PNG preview pane: pan the canvas left/right/up/down by a fixed
    /// fraction of the pane size. Render-time clamp keeps the canvas
    /// covering the pane.
    PreviewPngPanLeft,
    PreviewPngPanRight,
    PreviewPngPanUp,
    PreviewPngPanDown,
    /// Raster preview pane: toggle the dynamic physical scalebar overlay
    /// (ADR 0034). Only acts when the shown preview carries a physical scale.
    PreviewScalebarToggle,
    /// Sessions picker: commit the cursored directory as a new workspace,
    /// auto-starting the comm-aware agent (ccb) in its pane.
    SessionCreate,
    /// Sessions picker: commit the cursored directory as a new workspace
    /// with NO LLM agent — a bare shell/REPL session. Default Shift+Enter.
    SessionCreateBare,
    /// Sessions picker: commit the cursored directory as a new workspace
    /// auto-starting a CODEX session (ccx, ADR 0031). Default Ctrl+Enter.
    SessionCreateCodex,
    /// Nav focus: switch the root tree to Files mode. Default `f`.
    ModeFiles,
    /// Nav focus: switch the root tree to Modules mode. Default `m`.
    ModeModules,
    /// Nav focus: switch the root tree to Sessions mode. Default `s`.
    ModeSessions,
    /// Nav focus: switch the root tree to Hosts mode. Default `h`.
    ModeHosts,
    /// Nav focus (Files mode): toggle showing hidden dotfiles. Default `.`.
    ToggleHidden,
    /// Toggle the REPL drawer (focus into it / bounce back). Default Ctrl+j.
    ToggleReplDrawer,
    /// Toggle the Terminal drawer. Default Ctrl+t.
    ToggleTerminalDrawer,
    /// Toggle the Monitor drawer (ADR 0020). Default Ctrl+m.
    ToggleMonitorDrawer,
    /// Move pane focus to the pane left/right/up/down of the current one
    /// (4-way grid). Defaults Ctrl+Arrow{Left,Right,Up,Down}.
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,
    /// Cycle the active workspace forward/backward. Defaults Shift+Arrow
    /// Right/Left. Suppressed in edit mode (gated at the call site).
    WorkspaceCycleNext,
    WorkspaceCyclePrev,
    /// Quit the application. Default Ctrl+q (nav focus only at the call site).
    Quit,
    /// Toggle the keybindings help overlay. Default `?`.
    ToggleHelp,
    /// Toggle borderless fullscreen. Default F11.
    ToggleFullscreen,
    /// Collapse transport backoff and reconnect now. Default F5.
    Reconnect,
    /// Global font scale up / down / reset. Defaults Ctrl+= (and Ctrl++) /
    /// Ctrl+- (and Ctrl+_) / Ctrl+0.
    FontScaleUp,
    FontScaleDown,
    FontScaleReset,
    /// Capture the whole window to a timestamped PNG (a "selfie"). Fires from
    /// any pane. Default Ctrl+Shift+S.
    Selfie,
}

impl Action {
    fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "pane.maximize" | "maximize_pane" => Some(Action::MaximizePane),
            "pane.restore" | "restore_layout" => Some(Action::RestoreLayout),
            "preview.png.zoom_in" => Some(Action::PreviewPngZoomIn),
            "preview.png.zoom_out" => Some(Action::PreviewPngZoomOut),
            "preview.png.reset" => Some(Action::PreviewPngReset),
            "preview.png.pan_left" => Some(Action::PreviewPngPanLeft),
            "preview.png.pan_right" => Some(Action::PreviewPngPanRight),
            "preview.png.pan_up" => Some(Action::PreviewPngPanUp),
            "preview.png.pan_down" => Some(Action::PreviewPngPanDown),
            "preview.scalebar.toggle" => Some(Action::PreviewScalebarToggle),
            "session.create" => Some(Action::SessionCreate),
            "session.create_bare" => Some(Action::SessionCreateBare),
            "session.create_codex" => Some(Action::SessionCreateCodex),
            "mode.files" => Some(Action::ModeFiles),
            "mode.modules" => Some(Action::ModeModules),
            "mode.sessions" => Some(Action::ModeSessions),
            "mode.hosts" => Some(Action::ModeHosts),
            "files.toggle_hidden" => Some(Action::ToggleHidden),
            "drawer.repl" => Some(Action::ToggleReplDrawer),
            "drawer.terminal" => Some(Action::ToggleTerminalDrawer),
            "drawer.monitor" => Some(Action::ToggleMonitorDrawer),
            "focus.pane_left" => Some(Action::FocusPaneLeft),
            "focus.pane_right" => Some(Action::FocusPaneRight),
            "focus.pane_up" => Some(Action::FocusPaneUp),
            "focus.pane_down" => Some(Action::FocusPaneDown),
            "workspace.cycle_next" => Some(Action::WorkspaceCycleNext),
            "workspace.cycle_prev" => Some(Action::WorkspaceCyclePrev),
            "quit" => Some(Action::Quit),
            "help.toggle" => Some(Action::ToggleHelp),
            "view.fullscreen" => Some(Action::ToggleFullscreen),
            "transport.reconnect" => Some(Action::Reconnect),
            "font.scale_up" => Some(Action::FontScaleUp),
            "font.scale_down" => Some(Action::FontScaleDown),
            "font.scale_reset" => Some(Action::FontScaleReset),
            "view.selfie" => Some(Action::Selfie),
            _ => None,
        }
    }
}

/// Parsed chord — a key plus modifier flags.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    key: ChordKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChordKey {
    /// A typed character ("z", "=", "+", "-").
    Char(String),
    /// A named key ("Tab", "ArrowRight", "Escape", "Enter").
    Named(NamedKey),
}

impl Chord {
    /// Parse a chord string like "Alt+=" or "Ctrl+ArrowRight". Returns
    /// `None` for any malformed input — caller falls back to defaults.
    fn parse(s: &str) -> Option<Self> {
        // Single-character chords like "+" / "-" / "=" need to bypass the
        // `+`-delimited modifier parse — otherwise `split('+')` on "+"
        // gives ["", ""] which filters to empty and returns None.
        let trimmed = s.trim();
        if trimmed.chars().count() == 1 {
            return Some(Chord {
                ctrl: false,
                alt: false,
                shift: false,
                key: ChordKey::Char(trimmed.to_string()),
            });
        }
        // A literal "+" key after modifiers (e.g. "Ctrl++", "Ctrl+Shift++"):
        // `split('+')` would swallow it, so peel the trailing "+" off as the
        // key and parse what precedes the final separator '+' as modifiers.
        if let Some(mods) = trimmed.strip_suffix("++") {
            let mut ctrl = false;
            let mut alt = false;
            let mut shift = false;
            for m in mods.split('+').map(str::trim).filter(|p| !p.is_empty()) {
                match m.to_ascii_lowercase().as_str() {
                    "ctrl" | "control" => ctrl = true,
                    "alt" | "option" | "meta" => alt = true,
                    "shift" => shift = true,
                    _ => return None,
                }
            }
            return Some(Chord { ctrl, alt, shift, key: ChordKey::Char("+".to_string()) });
        }
        let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        for m in &parts[..parts.len() - 1] {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "option" | "meta" => alt = true,
                "shift" => shift = true,
                _ => return None,
            }
        }
        let last = parts[parts.len() - 1];
        let key = if last.chars().count() == 1 {
            ChordKey::Char(last.to_string())
        } else {
            ChordKey::Named(named_key_from_str(last)?)
        };
        Some(Chord { ctrl, alt, shift, key })
    }

    fn matches(&self, key: &Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        if self.ctrl != ctrl || self.alt != alt {
            return false;
        }
        // Shift is special: typing `=` may report shift=false on a US
        // layout but typing `+` reports shift=true. We only enforce
        // shift when the chord declares it, since the character itself
        // already encodes the shift state on most layouts.
        if self.shift && !shift {
            return false;
        }
        match (&self.key, key) {
            (ChordKey::Char(c), Key::Character(s)) => s.as_str().eq_ignore_ascii_case(c),
            (ChordKey::Named(n), Key::Named(k)) => n == k,
            _ => false,
        }
    }
}

fn named_key_from_str(s: &str) -> Option<NamedKey> {
    Some(match s {
        "Tab" => NamedKey::Tab,
        "Enter" | "Return" => NamedKey::Enter,
        "Escape" | "Esc" => NamedKey::Escape,
        "Space" => NamedKey::Space,
        "Backspace" => NamedKey::Backspace,
        "Delete" => NamedKey::Delete,
        "ArrowUp" | "Up" => NamedKey::ArrowUp,
        "ArrowDown" | "Down" => NamedKey::ArrowDown,
        "ArrowLeft" | "Left" => NamedKey::ArrowLeft,
        "ArrowRight" | "Right" => NamedKey::ArrowRight,
        "PageUp" => NamedKey::PageUp,
        "PageDown" => NamedKey::PageDown,
        "Home" => NamedKey::Home,
        "End" => NamedKey::End,
        "F1" => NamedKey::F1,
        "F2" => NamedKey::F2,
        "F3" => NamedKey::F3,
        "F4" => NamedKey::F4,
        "F5" => NamedKey::F5,
        "F6" => NamedKey::F6,
        "F7" => NamedKey::F7,
        "F8" => NamedKey::F8,
        "F9" => NamedKey::F9,
        "F10" => NamedKey::F10,
        "F11" => NamedKey::F11,
        "F12" => NamedKey::F12,
        _ => return None,
    })
}

/// Per-action chord list. Multiple chords per action let the user have
/// e.g. both `Alt+=` and `Ctrl+m` bound to maximise without losing one.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    maximize_pane: Vec<Chord>,
    restore_layout: Vec<Chord>,
    preview_png_zoom_in: Vec<Chord>,
    preview_png_zoom_out: Vec<Chord>,
    preview_png_reset: Vec<Chord>,
    preview_png_pan_left: Vec<Chord>,
    preview_png_pan_right: Vec<Chord>,
    preview_png_pan_up: Vec<Chord>,
    preview_png_pan_down: Vec<Chord>,
    preview_scalebar_toggle: Vec<Chord>,
    session_create: Vec<Chord>,
    session_create_bare: Vec<Chord>,
    session_create_codex: Vec<Chord>,
    mode_files: Vec<Chord>,
    mode_modules: Vec<Chord>,
    mode_sessions: Vec<Chord>,
    mode_hosts: Vec<Chord>,
    toggle_hidden: Vec<Chord>,
    toggle_repl_drawer: Vec<Chord>,
    toggle_terminal_drawer: Vec<Chord>,
    toggle_monitor_drawer: Vec<Chord>,
    focus_pane_left: Vec<Chord>,
    focus_pane_right: Vec<Chord>,
    focus_pane_up: Vec<Chord>,
    focus_pane_down: Vec<Chord>,
    workspace_cycle_next: Vec<Chord>,
    workspace_cycle_prev: Vec<Chord>,
    quit: Vec<Chord>,
    toggle_help: Vec<Chord>,
    toggle_fullscreen: Vec<Chord>,
    reconnect: Vec<Chord>,
    font_scale_up: Vec<Chord>,
    font_scale_down: Vec<Chord>,
    font_scale_reset: Vec<Chord>,
    selfie: Vec<Chord>,
}

impl KeyBindings {
    /// Built-in defaults. Used when no keybindings file is found.
    pub fn defaults() -> Self {
        // The default-set is small but should always parse; unwraps here
        // are tested by `defaults_parse_clean` below so a typo can't ship.
        Self {
            maximize_pane: vec![Chord::parse("Alt+=").unwrap()],
            // Esc restores the full layout — but only while a pane is
            // maximized (the dispatch guards on `state.maximized`), so Esc
            // still passes through to the pty / edit mode / etc. otherwise.
            restore_layout: vec![Chord::parse("Escape").unwrap()],
            // PNG preview zoom keeps the user's fingers on the arrow
            // cluster (Shift+ArrowUp/Down) and also supports the
            // muscle-memory `+`/`=`/`-` chords.
            preview_png_zoom_in: vec![
                Chord::parse("Shift+ArrowUp").unwrap(),
                Chord::parse("+").unwrap(),
                Chord::parse("=").unwrap(),
            ],
            preview_png_zoom_out: vec![
                Chord::parse("Shift+ArrowDown").unwrap(),
                Chord::parse("-").unwrap(),
            ],
            preview_png_reset: vec![
                Chord::parse("r").unwrap(),
                Chord::parse("0").unwrap(),
            ],
            preview_png_pan_left: vec![Chord::parse("ArrowLeft").unwrap()],
            preview_png_pan_right: vec![Chord::parse("ArrowRight").unwrap()],
            preview_png_pan_up: vec![Chord::parse("ArrowUp").unwrap()],
            preview_png_pan_down: vec![Chord::parse("ArrowDown").unwrap()],
            // Scalebar overlay toggle (ADR 0034). Ctrl+S per the maintainer
            // (2026-07-20) — `s` alone is the Sessions-mode switch, and
            // Ctrl+Shift+S is the selfie, so Ctrl+S is the free slot. With no
            // physical scale present this opens the pixel-size prompt rather
            // than no-opping.
            preview_scalebar_toggle: vec![Chord::parse("Ctrl+s").unwrap()],
            // Sessions picker commit: Enter = with agent (ccb), Shift+Enter =
            // bare (no LLM). The call site checks `session.create_bare` before
            // `session.create`, since a non-shift "Enter" chord also matches
            // when shift is held.
            session_create: vec![Chord::parse("Enter").unwrap()],
            session_create_bare: vec![Chord::parse("Shift+Enter").unwrap()],
            session_create_codex: vec![Chord::parse("Ctrl+Enter").unwrap()],
            // Mode switches (nav focus only — gated at the call site, so these
            // single-char chords stay literal text in the pty/edit contexts).
            mode_files: vec![Chord::parse("f").unwrap()],
            mode_modules: vec![Chord::parse("m").unwrap()],
            mode_sessions: vec![Chord::parse("s").unwrap()],
            mode_hosts: vec![Chord::parse("h").unwrap()],
            // Show/hide hidden dotfiles in Files mode (nav focus only — the
            // single-char chord stays literal text in the pty/edit contexts).
            toggle_hidden: vec![Chord::parse(".").unwrap()],
            // Drawer toggles (global).
            toggle_repl_drawer: vec![Chord::parse("Ctrl+j").unwrap()],
            toggle_terminal_drawer: vec![Chord::parse("Ctrl+t").unwrap()],
            toggle_monitor_drawer: vec![Chord::parse("Ctrl+m").unwrap()],
            // Spatial pane focus (global). Ctrl+Arrow keeps it disjoint from
            // plain arrows (per-pane nav) and Shift+Arrow (workspace cycle).
            focus_pane_left: vec![Chord::parse("Ctrl+ArrowLeft").unwrap()],
            focus_pane_right: vec![Chord::parse("Ctrl+ArrowRight").unwrap()],
            focus_pane_up: vec![Chord::parse("Ctrl+ArrowUp").unwrap()],
            focus_pane_down: vec![Chord::parse("Ctrl+ArrowDown").unwrap()],
            // Workspace cycle (global, suppressed in edit mode at the call site).
            workspace_cycle_next: vec![Chord::parse("Shift+ArrowRight").unwrap()],
            workspace_cycle_prev: vec![Chord::parse("Shift+ArrowLeft").unwrap()],
            // Global chrome.
            quit: vec![Chord::parse("Ctrl+q").unwrap()],
            toggle_help: vec![Chord::parse("?").unwrap()],
            toggle_fullscreen: vec![Chord::parse("F11").unwrap()],
            reconnect: vec![Chord::parse("F5").unwrap()],
            // "=" and shifted "+" both zoom in; "-" and shifted "_" both zoom
            // out (matches the prior hardcoded behaviour on US layouts).
            font_scale_up: vec![
                Chord::parse("Ctrl+=").unwrap(),
                Chord::parse("Ctrl++").unwrap(),
            ],
            font_scale_down: vec![
                Chord::parse("Ctrl+-").unwrap(),
                Chord::parse("Ctrl+_").unwrap(),
            ],
            font_scale_reset: vec![Chord::parse("Ctrl+0").unwrap()],
            // Whole-window screenshot to a timestamped PNG (fires from any pane).
            selfie: vec![Chord::parse("Ctrl+Shift+S").unwrap()],
        }
    }

    /// Layered load: defaults overlaid with whatever
    /// `find_keybindings_file()` returns. A failed load logs at warn
    /// level and falls back to defaults — the chrome should never crash
    /// just because the user wrote a malformed config.
    pub fn load_layered() -> Self {
        let mut bindings = Self::defaults();
        if let Some(path) = find_keybindings_file() {
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    bindings.merge_text(&contents);
                    tracing::info!(path = %path.display(), "keybindings loaded");
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e,
                        "failed to read keybindings file; using defaults");
                }
            }
        }
        bindings
    }

    /// Apply a parsed keybindings file on top of `self`. Each recognised
    /// action wholly replaces the default chord list (so `Alt+=` is
    /// dropped if the user writes `pane.maximize = "Ctrl+m"`); writing
    /// a list `["a", "b"]` keeps both. Unknown actions and unparseable
    /// chord strings are warned and skipped.
    fn merge_text(&mut self, contents: &str) {
        for (lineno, raw) in contents.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            let Some(action) = Action::parse(key) else {
                tracing::warn!(line = lineno + 1, key, "unknown action in keybindings");
                continue;
            };
            let chord_strs = parse_value(value);
            let mut chords = Vec::new();
            for s in chord_strs {
                if let Some(c) = Chord::parse(&s) {
                    chords.push(c);
                } else {
                    tracing::warn!(line = lineno + 1, chord = %s,
                        "unparseable chord; skipping");
                }
            }
            if !chords.is_empty() {
                match action {
                    Action::MaximizePane => self.maximize_pane = chords,
                    Action::RestoreLayout => self.restore_layout = chords,
                    Action::PreviewPngZoomIn => self.preview_png_zoom_in = chords,
                    Action::PreviewPngZoomOut => self.preview_png_zoom_out = chords,
                    Action::PreviewPngReset => self.preview_png_reset = chords,
                    Action::PreviewPngPanLeft => self.preview_png_pan_left = chords,
                    Action::PreviewPngPanRight => self.preview_png_pan_right = chords,
                    Action::PreviewPngPanUp => self.preview_png_pan_up = chords,
                    Action::PreviewPngPanDown => self.preview_png_pan_down = chords,
                    Action::PreviewScalebarToggle => self.preview_scalebar_toggle = chords,
                    Action::SessionCreate => self.session_create = chords,
                    Action::SessionCreateBare => self.session_create_bare = chords,
                    Action::SessionCreateCodex => self.session_create_codex = chords,
                    Action::ModeFiles => self.mode_files = chords,
                    Action::ModeModules => self.mode_modules = chords,
                    Action::ModeSessions => self.mode_sessions = chords,
                    Action::ModeHosts => self.mode_hosts = chords,
                    Action::ToggleHidden => self.toggle_hidden = chords,
                    Action::ToggleReplDrawer => self.toggle_repl_drawer = chords,
                    Action::ToggleTerminalDrawer => self.toggle_terminal_drawer = chords,
                    Action::ToggleMonitorDrawer => self.toggle_monitor_drawer = chords,
                    Action::FocusPaneLeft => self.focus_pane_left = chords,
                    Action::FocusPaneRight => self.focus_pane_right = chords,
                    Action::FocusPaneUp => self.focus_pane_up = chords,
                    Action::FocusPaneDown => self.focus_pane_down = chords,
                    Action::WorkspaceCycleNext => self.workspace_cycle_next = chords,
                    Action::WorkspaceCyclePrev => self.workspace_cycle_prev = chords,
                    Action::Quit => self.quit = chords,
                    Action::ToggleHelp => self.toggle_help = chords,
                    Action::ToggleFullscreen => self.toggle_fullscreen = chords,
                    Action::Reconnect => self.reconnect = chords,
                    Action::FontScaleUp => self.font_scale_up = chords,
                    Action::FontScaleDown => self.font_scale_down = chords,
                    Action::FontScaleReset => self.font_scale_reset = chords,
                    Action::Selfie => self.selfie = chords,
                }
            }
        }
    }

    /// Does the runtime key event match any chord bound to `action`?
    pub fn matches(&self, action: Action, key: &Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        let list = match action {
            Action::MaximizePane => &self.maximize_pane,
            Action::RestoreLayout => &self.restore_layout,
            Action::PreviewPngZoomIn => &self.preview_png_zoom_in,
            Action::PreviewPngZoomOut => &self.preview_png_zoom_out,
            Action::PreviewPngReset => &self.preview_png_reset,
            Action::PreviewPngPanLeft => &self.preview_png_pan_left,
            Action::PreviewPngPanRight => &self.preview_png_pan_right,
            Action::PreviewPngPanUp => &self.preview_png_pan_up,
            Action::PreviewPngPanDown => &self.preview_png_pan_down,
            Action::PreviewScalebarToggle => &self.preview_scalebar_toggle,
            Action::SessionCreate => &self.session_create,
            Action::SessionCreateBare => &self.session_create_bare,
            Action::SessionCreateCodex => &self.session_create_codex,
            Action::ModeFiles => &self.mode_files,
            Action::ModeModules => &self.mode_modules,
            Action::ModeSessions => &self.mode_sessions,
            Action::ModeHosts => &self.mode_hosts,
            Action::ToggleHidden => &self.toggle_hidden,
            Action::ToggleReplDrawer => &self.toggle_repl_drawer,
            Action::ToggleTerminalDrawer => &self.toggle_terminal_drawer,
            Action::ToggleMonitorDrawer => &self.toggle_monitor_drawer,
            Action::FocusPaneLeft => &self.focus_pane_left,
            Action::FocusPaneRight => &self.focus_pane_right,
            Action::FocusPaneUp => &self.focus_pane_up,
            Action::FocusPaneDown => &self.focus_pane_down,
            Action::WorkspaceCycleNext => &self.workspace_cycle_next,
            Action::WorkspaceCyclePrev => &self.workspace_cycle_prev,
            Action::Quit => &self.quit,
            Action::ToggleHelp => &self.toggle_help,
            Action::ToggleFullscreen => &self.toggle_fullscreen,
            Action::Reconnect => &self.reconnect,
            Action::FontScaleUp => &self.font_scale_up,
            Action::FontScaleDown => &self.font_scale_down,
            Action::FontScaleReset => &self.font_scale_reset,
            Action::Selfie => &self.selfie,
        };
        list.iter().any(|c| c.matches(key, ctrl, alt, shift))
    }
}

/// Pull a value off the right of `key = ...`. Supports a single quoted
/// string (`"Alt+="`), a bare token (`Alt+=`), or a list (`["a", "b"]`).
/// Returns each chord string with its quotes stripped.
fn parse_value(v: &str) -> Vec<String> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        inner
            .split(',')
            .map(str::trim)
            .map(strip_quotes)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![strip_quotes(v)]
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

fn find_keybindings_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOT_KEYBINDINGS") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    // repo-local: walk up from cwd looking for .sot/keybindings.toml.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: &Path = &cwd;
        loop {
            let candidate = cur.join(".sot").join("keybindings.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("sot")
            .join("keybindings.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_clean() {
        // The defaults() unwraps must never fire — guard here.
        let _ = KeyBindings::defaults();
    }

    #[test]
    fn chord_parses_alt_equals() {
        let c = Chord::parse("Alt+=").unwrap();
        assert!(!c.ctrl);
        assert!(c.alt);
        assert_eq!(c.key, ChordKey::Char("=".into()));
    }

    #[test]
    fn chord_parses_named() {
        let c = Chord::parse("Ctrl+ArrowRight").unwrap();
        assert!(c.ctrl);
        assert_eq!(c.key, ChordKey::Named(NamedKey::ArrowRight));
    }

    #[test]
    fn chord_parses_function_key() {
        let c = Chord::parse("F11").unwrap();
        assert!(!c.ctrl && !c.alt && !c.shift);
        assert_eq!(c.key, ChordKey::Named(NamedKey::F11));
    }

    /// ADR 0034: the scalebar toggle is Ctrl+S (maintainer, 2026-07-20). Pin it
    /// against the two neighbours that make it a live collision risk — bare `s`
    /// is the Sessions-mode switch and Ctrl+Shift+S is the selfie — so a future
    /// rebind can't silently make one of them fire the scalebar (or vice versa).
    #[test]
    fn scalebar_toggle_is_ctrl_s_and_does_not_collide() {
        let b = KeyBindings::defaults();
        let s = Key::Character("s".into());

        // Ctrl+S fires the toggle.
        assert!(b.matches(Action::PreviewScalebarToggle, &s, true, false, false));
        // Bare `s` does NOT (that's Sessions mode).
        assert!(!b.matches(Action::PreviewScalebarToggle, &s, false, false, false));
        // ...and bare `s` still reaches Sessions mode.
        assert!(b.matches(Action::ModeSessions, &s, false, false, false));
        // Ctrl+S must not fire Sessions mode.
        assert!(!b.matches(Action::ModeSessions, &s, true, false, false));
        // Ctrl+Shift+S is the selfie, not the scalebar.
        assert!(b.matches(Action::Selfie, &s, true, false, true));
    }

    #[test]
    fn chord_parses_literal_plus_after_modifier() {
        // "Ctrl++" — the trailing literal '+' must survive the '+'-delimited
        // modifier split (the font-zoom-in chord on US layouts).
        let c = Chord::parse("Ctrl++").unwrap();
        assert!(c.ctrl && !c.alt && !c.shift);
        assert_eq!(c.key, ChordKey::Char("+".into()));
        let plus = Key::Character("+".into());
        assert!(c.matches(&plus, true, false, false));
    }

    #[test]
    fn merge_overrides_default() {
        let mut b = KeyBindings::defaults();
        b.merge_text("[keys]\npane.maximize = \"Ctrl+m\"\n");
        let key = Key::Character("m".into());
        assert!(b.matches(Action::MaximizePane, &key, true, false, false));
        // Default Alt+= is replaced, not extended:
        let alt_eq = Key::Character("=".into());
        assert!(!b.matches(Action::MaximizePane, &alt_eq, false, true, false));
    }

    #[test]
    fn merge_supports_list() {
        let mut b = KeyBindings::defaults();
        b.merge_text("pane.maximize = [\"Alt+=\", \"Ctrl+m\"]\n");
        let m = Key::Character("m".into());
        let eq = Key::Character("=".into());
        assert!(b.matches(Action::MaximizePane, &m, true, false, false));
        assert!(b.matches(Action::MaximizePane, &eq, false, true, false));
    }

    #[test]
    fn restore_default_is_escape_not_alt_minus() {
        let b = KeyBindings::defaults();
        let esc = Key::Named(NamedKey::Escape);
        assert!(b.matches(Action::RestoreLayout, &esc, false, false, false));
        // Alt+- is no longer the restore binding.
        let minus = Key::Character("-".into());
        assert!(!b.matches(Action::RestoreLayout, &minus, false, true, false));
    }

    #[test]
    fn unknown_action_ignored() {
        let mut b = KeyBindings::defaults();
        b.merge_text("nonsense.action = \"x\"\n");
        // Defaults still intact.
        let eq = Key::Character("=".into());
        assert!(b.matches(Action::MaximizePane, &eq, false, true, false));
    }

    #[test]
    fn selfie_default_is_ctrl_shift_s() {
        let b = KeyBindings::defaults();
        let s = Key::Character("S".into());
        assert!(b.matches(Action::Selfie, &s, true, false, true));
        // Lowercase (caps-lock / layouts that don't upcase) still matches.
        let lower = Key::Character("s".into());
        assert!(b.matches(Action::Selfie, &lower, true, false, true));
        // Ctrl without Shift must NOT trigger it.
        assert!(!b.matches(Action::Selfie, &s, true, false, false));
    }
}
