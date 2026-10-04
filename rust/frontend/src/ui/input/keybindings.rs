// One action catalog supplies dispatch, resolved shortcut labels and contextual help.
use std::collections::HashMap;
use std::fs;
use crate::ui::persist::discover::find_config_file;
use winit::keyboard::{Key, NamedKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    /// Files-mode navigation OR the new-session folder picker: the one
    /// action that means the same thing in both (hidden entries).
    FilesOrPicker,
    Workspace,
    Restore,
    Scroll,
    Text,
    Picker,
    Nav,
    FileNav,
    Files,
    Navigation,
    Session,
    File,
    Quarto,
    Julia,
    Repl,
    Pty,
    Llm,
    Pages,
    Image,
    Reading,
    PreviewFile,
    PageScroll,
    Edit,
    Modal,
    Prompt,
    HalfScroll,
    Help,
    DeleteConfirm,
    DiscardConfirm,
    StaleEdit,
}

pub struct ActionSpec {
    pub action: Action,
    pub name: &'static str,
    pub defaults: &'static [&'static str],
    pub label: &'static str,
    pub detail: &'static str,
    pub group: &'static str,
    pub scope: Scope,
}
macro_rules! actions {
    ($( $action:ident, $name:literal, [$($key:literal),*], $label:literal, $detail:literal, $group:literal, $scope:ident; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($action),* }
        pub static ACTIONS: &[ActionSpec] = &[$(ActionSpec {
            action: Action::$action, name: $name, defaults: &[$($key),*],
            label: $label, detail: $detail, group: $group, scope: Scope::$scope,
        }),*];
        impl Action {
            pub fn spec(self) -> &'static ActionSpec { &ACTIONS[self as usize] }
            fn parse(name: &str) -> Option<Self> {
                match name.trim() {
                    "maximize_pane" => Some(Self::MaximizePane),
                    "restore_layout" => Some(Self::RestoreLayout),
                    $($name => Some(Self::$action),)*
                    _ => None,
                }
            }
        }
    }
}
actions! {
    ToggleHelp, "help.toggle", ["Primary+?"], "Pane actions", "Show actions here for five seconds; press again to browse them in the Help drawer.", "Help", Global;
    ToggleHelpDrawer, "drawer.help", ["F1"], "Help drawer", "Browse and search actions for the pane you were using. Escape restores the previous drawer.", "Help", Global;
    Reconnect, "transport.reconnect", ["F5"], "Reconnect", "Retry backend connections immediately.", "Window", Global;
    ToggleFullscreen, "view.fullscreen", ["F11"], "Fullscreen", "Toggle borderless fullscreen.", "Window", Global;
    FontScaleUp, "font.scale_up", ["Primary+=", "Primary++"], "Larger text", "Increase the frontend font size.", "Window", Global;
    FontScaleDown, "font.scale_down", ["Primary+-", "Primary+_"], "Smaller text", "Decrease the frontend font size.", "Window", Global;
    FontScaleReset, "font.scale_reset", ["Primary+0"], "Reset text size", "Restore the default frontend font size.", "Window", Global;
    FocusPaneLeft, "focus.pane_left", ["Primary+ArrowLeft"], "Focus left", "Move keyboard focus to the pane on the left.", "Window", Global;
    FocusPaneRight, "focus.pane_right", ["Primary+ArrowRight"], "Focus right", "Move keyboard focus to the pane on the right.", "Window", Global;
    FocusPaneUp, "focus.pane_up", ["Primary+ArrowUp"], "Focus above", "Move keyboard focus to the pane above.", "Window", Global;
    FocusPaneDown, "focus.pane_down", ["Primary+ArrowDown"], "Focus below", "Move keyboard focus to the drawer or pane below.", "Window", Global;
    WorkspaceCycleNext, "workspace.cycle_next", ["Shift+ArrowRight"], "Next session", "Switch to the next workspace.", "Window", Workspace;
    WorkspaceCyclePrev, "workspace.cycle_prev", ["Shift+ArrowLeft"], "Previous session", "Switch to the previous workspace.", "Window", Workspace;
    MaximizePane, "pane.maximize", ["Alt+="], "Maximize pane", "Fill the window with the focused pane.", "Window", Global;
    RestoreLayout, "pane.restore", ["Escape"], "Restore layout", "Undo maximization, then wide preview, one layer at a time.", "Window", Restore;
    ToggleWidePreview, "layout.wide_preview", ["Alt++"], "Wide preview", "Hide or show the agent column to give the preview more room.", "Window", Global;
    Selfie, "view.selfie", ["Primary+Shift+S"], "Screenshot", "Save a PNG of the whole frontend window.", "Window", Global;
    ToggleReplDrawer, "drawer.repl", ["Primary+j"], "Julia drawer", "Show or hide the Julia REPL. Changing drawer views keeps Julia running.", "Drawers", Global;
    ToggleTerminalDrawer, "drawer.terminal", ["Primary+t"], "Terminal drawer", "Show or hide the frontend-local terminal.", "Drawers", Global;
    ToggleMonitorDrawer, "drawer.monitor", ["Primary+m"], "Monitor drawer", "Show or hide host resource charts.", "Drawers", Global;
    ScrollLineUp, "view.scroll_line_up", ["Alt+ArrowUp"], "Scroll up one line", "Scroll without moving the input cursor.", "Scroll", Scroll;
    ScrollLineDown, "view.scroll_line_down", ["Alt+ArrowDown"], "Scroll down one line", "Scroll without moving the input cursor.", "Scroll", Scroll;
    TableLeft, "preview.table_left", ["h", "ArrowLeft"], "Scroll left", "Move horizontally through a wide text table.", "Text", Text;
    TableRight, "preview.table_right", ["l", "ArrowRight"], "Scroll right", "Move horizontally through a wide text table.", "Text", Text;
    TableReset, "preview.table_reset", ["0"], "Table start", "Return to the left edge of a wide table.", "Text", Text;
    SessionCreateCodex, "session.create_codex", ["Primary+Enter"], "Create with Codex", "Create a workspace in the selected folder with a Codex agent.", "Picker", Picker;
    SessionCreateBare, "session.create_bare", ["Shift+Enter"], "Create without agent", "Create a workspace in the selected folder with a shell and no agent.", "Picker", Picker;
    SessionCreate, "session.create", ["Enter"], "Create with Claude", "Create a workspace in the selected folder with a Claude Code agent.", "Picker", Picker;
    SessionAccountNext, "session.account_next", ["Tab"], "Next account", "Cycle which login account the new session will use. Hidden when only one account is discovered.", "Picker", Picker;
    Quit, "quit", ["Primary+q"], "Quit SoT", "Close the window from navigation focus, asking whether to keep the daemon and sessions running.", "Navigation", Nav;
    CopyPath, "files.copy_path", ["Primary+c", "c"], "Copy path", "Copy the selected file's backend path to the clipboard.", "Files", FileNav;
    NewFile, "files.new", ["Primary+n"], "New file or folder", "Create a file, or a folder when the name ends with /, in the selected directory.", "Files", Files;
    DeleteFile, "files.delete", ["Primary+d"], "Delete file", "Delete the selected file after confirmation. Directories are refused.", "Files", FileNav;
    NavDown, "nav.down", ["ArrowDown"], "Move down", "Select the next row.", "Navigation", Navigation;
    NavUp, "nav.up", ["ArrowUp"], "Move up", "Select the previous row.", "Navigation", Navigation;
    NavExpand, "nav.expand", ["ArrowRight"], "Expand", "Expand the selected node or descend into a folder.", "Navigation", Navigation;
    NavOpen, "nav.open", ["Enter"], "Open or select", "Open the selected node; in Sessions or Hosts, select that target.", "Navigation", Nav;
    NavCollapse, "nav.collapse", ["ArrowLeft"], "Collapse", "Collapse the node or go to its parent.", "Navigation", Navigation;
    ModeFiles, "mode.files", ["f"], "Files mode", "Browse project files.", "Navigation", Nav;
    ModeModules, "mode.modules", ["m"], "Modules mode", "Browse Julia modules and methods.", "Navigation", Nav;
    ModeSessions, "mode.sessions", ["s"], "Sessions mode", "Browse workspaces and agent sessions.", "Navigation", Nav;
    TogglePin, "files.pin", ["p"], "Pin or unpin preview", "Keep the selected file in preview while browsing. Unpin returns to the pinned file.", "Files", Files;
    ModeHosts, "mode.hosts", ["h"], "Hosts mode", "Browse backend hosts.", "Navigation", Nav;
    ToggleHidden, "files.toggle_hidden", ["."], "Hidden files", "Show or hide dotfiles here or in the folder picker (a hidden folder can be a workspace root).", "Files", FilesOrPicker;
    SessionDestroy, "session.destroy", ["Shift+d"], "Destroy session", "Press twice to destroy the selected session. Any other command cancels confirmation.", "Sessions", Session;
    OpenExternal, "file.open", ["o"], "Open externally", "Open HTML, video or an interactive document in the browser; Julia files open in Pluto.", "Files", File;
    OpenDocs, "file.docs", ["Shift+w"], "Open built docs", "Open the built documentation site for this file in the browser.", "Files", File;
    OpenExecute, "file.execute", ["Shift+o"], "Render and execute", "Render a Quarto document and execute its code chunks.", "Files", Quarto;
    Download, "files.download", ["d"], "Download file", "Download the selected file to the frontend machine.", "Files", FileNav;
    Upload, "files.upload", ["u"], "Upload files", "Choose local files to upload into the selected directory.", "Files", Files;
    RunFresh, "files.run_fresh", ["r"], "Run in fresh REPL", "Restart Julia in this project and run the selected Julia file. Existing REPL variables are cleared.", "Julia", Julia;
    RunCurrent, "files.run_current", ["Shift+r"], "Run in current REPL", "Run the selected Julia file using the current Julia session and its variables.", "Julia", Julia;
    ReplClear, "repl.clear", ["Ctrl+l"], "Clear scrollback", "Clear the displayed REPL output; Julia's variables remain.", "Julia", Repl;
    Paste, "input.paste", ["Ctrl+v", "Super+v", "Shift+Insert"], "Paste", "Paste the clipboard into this pane.", "Input", Pty;
    CopySelection, "agent.copy", ["Primary+Shift+c"], "Copy selection", "Copy selected agent output to the clipboard.", "Agent", Llm;
    PageNext, "preview.page_next", ["n", "PageDown"], "Next page", "Show the next page of the displayed document.", "Pages", Pages;
    PagePrev, "preview.page_prev", ["p", "PageUp"], "Previous page", "Show the previous page of the displayed document.", "Pages", Pages;
    PreviewPngReset, "preview.png.reset", ["r", "0"], "Fit image", "Reset zoom and pan to fit the image in the pane.", "Image", Image;
    PreviewPngZoomIn, "preview.png.zoom_in", ["Shift+ArrowUp", "+", "="], "Zoom in", "Enlarge the displayed image. Fit image restores the overview.", "Image", Image;
    PreviewPngZoomOut, "preview.png.zoom_out", ["Shift+ArrowDown", "-"], "Zoom out", "Reduce image zoom down to fit-to-pane.", "Image", Image;
    PreviewPngPanLeft, "preview.png.pan_left", ["ArrowLeft"], "Pan left", "Move across the zoomed image.", "Image", Image;
    PreviewPngPanRight, "preview.png.pan_right", ["ArrowRight"], "Pan right", "Move across the zoomed image.", "Image", Image;
    PreviewPngPanUp, "preview.png.pan_up", ["ArrowUp"], "Pan up", "Move across the zoomed image.", "Image", Image;
    PreviewPngPanDown, "preview.png.pan_down", ["ArrowDown"], "Pan down", "Move across the zoomed image.", "Image", Image;
    PreviewScalebarToggle, "preview.scalebar.toggle", ["Primary+s"], "Scalebar", "Toggle the physical scalebar, or enter the pixel size when scale is unknown.", "Image", Image;
    ReturnNav, "view.return_nav", ["Escape"], "Return to navigation", "Move focus back to the navigation tree.", "View", Reading;
    CaptureRegion, "preview.capture", ["c", "Primary+c"], "Send visible region", "Crop the visible image region and attach it to the agent input.", "Image", Image;
    CopyCode, "preview.copy_code", ["y"], "Copy code blocks", "Copy the displayed markdown code blocks to the clipboard.", "Text", Text;
    EditFile, "preview.edit", ["e"], "Edit", "Edit the displayed file or its concept annotation.", "Text", PreviewFile;
    ScrollPageUp, "view.page_up", ["PageUp"], "Scroll up", "Scroll back through this pane.", "Scroll", PageScroll;
    ScrollPageDown, "view.page_down", ["PageDown"], "Scroll down", "Scroll forward through this pane.", "Scroll", PageScroll;
    PreviewUp, "preview.up", ["ArrowUp"], "Scroll up one line", "Move up through the displayed text.", "Text", Text;
    PreviewDown, "preview.down", ["ArrowDown"], "Scroll down one line", "Move down through the displayed text.", "Text", Text;
    PreviewStart, "preview.start", ["Home"], "Document start", "Scroll to the beginning of the displayed text.", "Text", Text;
    PreviewEnd, "preview.end", ["End"], "Document end", "Scroll to the end of the displayed text.", "Text", Text;
    PreviewHalfUp, "preview.half_up", ["Ctrl+u"], "Half page up", "Scroll up half a pane.", "Text", HalfScroll;
    PreviewHalfDown, "preview.half_down", ["Ctrl+d"], "Half page down", "Scroll down half a pane.", "Text", HalfScroll;
    HelpUp, "help.up", ["ArrowUp"], "Previous action", "Previous action in the Help drawer.", "Help", Help;
    HelpDown, "help.down", ["ArrowDown"], "Next action", "Next action in the Help drawer.", "Help", Help;
    HelpPageUp, "help.page_up", ["PageUp"], "Previous actions", "Previous actions in the Help drawer.", "Help", Help;
    HelpPageDown, "help.page_down", ["PageDown"], "Next actions", "Next actions in the Help drawer.", "Help", Help;
    HelpScope, "help.scope", ["Tab"], "This pane or all panes", "This pane or all panes in the Help drawer.", "Help", Help;
    HelpManual, "help.manual", ["Enter"], "Open manual", "Open manual in the Help drawer.", "Help", Help;
    HelpCheatSheet, "help.cheatsheet", ["Primary+p"], "Print cheat sheet", "Write a printable page of every listed action and its loaded shortcuts, and open it in the browser (print or save as PDF there).", "Help", Help;
    HelpClose, "help.close", ["Escape"], "Return to work", "Return to work in the Help drawer.", "Help", Help;
    DeleteConfirm, "files.confirm_delete", ["y", "Shift+y"], "Confirm deletion", "Delete the file named in the confirmation prompt. Any other key cancels.", "Input", DeleteConfirm;
    DiscardConfirm, "edit.confirm_discard", ["y", "Shift+y", "Escape"], "Discard edits", "Discard unsaved edits and close the editor. Another key returns to editing.", "Editor", DiscardConfirm;
    StaleReload, "edit.reload", ["r", "Shift+r"], "Reload from disk", "Discard this edit buffer and reload the externally changed file.", "Editor", StaleEdit;
    StaleKeep, "edit.keep", ["k", "Shift+k"], "Keep editing", "Dismiss the external-change warning and continue editing.", "Editor", StaleEdit;
    EditUndo, "edit.undo", ["Ctrl+z", "Super+z"], "Undo", "Undo in the editor.", "Editor", Edit;
    EditRedo, "edit.redo", ["Ctrl+y", "Super+Shift+z"], "Redo", "Redo in the editor.", "Editor", Edit;
    EditCopy, "edit.copy", ["Ctrl+c", "Super+c"], "Copy selection", "Copy selection in the editor.", "Editor", Edit;
    EditCut, "edit.cut", ["Ctrl+x", "Super+x"], "Cut selection", "Cut selection in the editor.", "Editor", Edit;
    EditPaste, "edit.paste", ["Ctrl+v", "Super+v", "Shift+Insert"], "Paste", "Paste in the editor.", "Editor", Edit;
    EditNewline, "edit.newline", ["Enter"], "Insert newline", "Insert newline in the editor.", "Editor", Edit;
    EditBackspace, "edit.backspace", ["Backspace"], "Delete previous character", "Delete previous character in the editor.", "Editor", Edit;
    EditDelete, "edit.delete", ["Delete"], "Delete next character", "Delete next character in the editor.", "Editor", Edit;
    EditLeft, "edit.left", ["ArrowLeft", "Shift+ArrowLeft"], "Move left", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditRight, "edit.right", ["ArrowRight", "Shift+ArrowRight"], "Move right", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditUp, "edit.up", ["ArrowUp", "Shift+ArrowUp"], "Move up", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditDown, "edit.down", ["ArrowDown", "Shift+ArrowDown"], "Move down", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditHome, "edit.home", ["Home", "Shift+Home"], "Line start", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditEnd, "edit.end", ["End", "Shift+End"], "Line end", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditPageUp, "edit.page_up", ["PageUp", "Shift+PageUp"], "Page up", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditPageDown, "edit.page_down", ["PageDown", "Shift+PageDown"], "Page down", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditStart, "edit.start", ["Ctrl+Home", "Shift+Ctrl+Home"], "Document start", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditFinish, "edit.finish", ["Ctrl+End", "Shift+Ctrl+End"], "Document end", "Move the editor cursor. Hold Shift to extend the selection.", "Editor", Edit;
    EditSave, "edit.save", ["Ctrl+s", "Super+s"], "Save edits", "Save the current file or concept annotation.", "Editor", Edit;
    Cancel, "input.cancel", ["Escape"], "Cancel or close", "Cancel the current input. Unsaved edits require confirmation.", "Input", Modal;
    Confirm, "input.confirm", ["Enter"], "Confirm input", "Submit the entered filename or value.", "Input", Prompt;
    ReplInterrupt, "repl.interrupt", ["Ctrl+c"], "Interrupt or clear input", "Interrupt the current evaluation, or clear the input when Julia is idle.", "Julia", Repl;
    ReplHistoryPrev, "repl.history_prev", ["ArrowUp"], "Previous input", "Recall the previous Julia input.", "Julia", Repl;
    ReplHistoryNext, "repl.history_next", ["ArrowDown"], "Next input", "Recall the next Julia input.", "Julia", Repl;
    PickerParent, "picker.parent", ["Backspace"], "Parent folder", "Go to the parent folder in the new-session picker.", "Picker", Picker;
    ReplSubmit, "repl.submit", ["Enter"], "Evaluate", "Evaluate the current Julia input.", "Julia", Repl;
    ReplNewline, "repl.newline", ["Shift+Enter"], "Insert newline", "Continue the Julia input on another line.", "Julia", Repl;
}

/// Modifier identity is independent of the OS glyph used to display it.
/// The US layout's shifted symbol for an unshifted printable key. Consulted
/// only when a platform hands over the unshifted key while Shift is held with
/// a modifier (Windows under Ctrl/Alt); see `Chord::matches_input`.
fn us_shifted(base: &str) -> Option<&'static str> {
    Some(match base {
        "/" => "?", "=" => "+", "-" => "_", ";" => ":", "'" => "\"", "," => "<", "." => ">",
        "[" => "{", "]" => "}", "\\" => "|", "`" => "~",
        "1" => "!", "2" => "@", "3" => "#", "4" => "$", "5" => "%", "6" => "^", "7" => "&",
        "8" => "*", "9" => "(", "0" => ")",
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_: bool,
    key: ChordKey,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChordKey {
    Char(String),
    Named(NamedKey),
}
impl Chord {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (mods, key) = if s == "+" {
            ("", "+")
        } else if let Some(m) = s.strip_suffix("++") {
            (m, "+")
        } else {
            s.rsplit_once('+').unwrap_or(("", s))
        };
        let mut chord = Self {
            ctrl: false,
            alt: false,
            shift: false,
            super_: false,
            key: if key.chars().count() == 1 {
                ChordKey::Char(key.into())
            } else {
                ChordKey::Named(named_key_from_str(key)?)
            },
        };
        for m in mods.split('+').filter(|s| !s.is_empty()) {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "meta" => chord.alt = true, // legacy Meta alias
                "shift" => chord.shift = true,
                "super" | "cmd" | "command" | "win" => chord.super_ = true,
                "primary" => {
                    if cfg!(target_os = "macos") {
                        chord.super_ = true
                    } else {
                        chord.ctrl = true
                    }
                }
                _ => return None,
            }
        }
        // A bare uppercase character is a shifted action (r and R stay distinct).
        if !chord.ctrl
            && !chord.alt
            && !chord.super_
            && key.chars().any(|c| c.is_ascii_uppercase())
            && key.len() == 1
        {
            chord.shift = true;
        }
        Some(chord)
    }
    /// No Ctrl/Alt/Super and a printable key: pressing it TYPES a character.
    /// Such a chord (a user override like `help.toggle = "?"`) must never
    /// fire where the pane consumes typed text -- the old keymap's
    /// "nav-focus-gated so it stays literal text in the pty/editor/prompts"
    /// invariant, kept in the resolver so Help's labels agree with dispatch.
    fn is_bare_text(&self) -> bool {
        !self.ctrl && !self.alt && !self.super_ && matches!(self.key, ChordKey::Char(_))
    }
    fn matches_input(&self, key: &Key, base: Option<&Key>, m: Modifiers) -> bool {
        self.matches_input_on(key, base, m, cfg!(target_os = "macos"))
    }
    /// `mac` is threaded through as a plain argument (not read from `cfg!`
    /// inline) so tests can exercise the macOS-only branch below from any
    /// host; the one production call site above always passes the real OS.
    fn matches_input_on(&self, key: &Key, base: Option<&Key>, m: Modifiers, mac: bool) -> bool {
        if (self.ctrl, self.alt, self.super_) != (m.ctrl, m.alt, m.super_) {
            return false;
        }
        match (&self.key, key) {
            (ChordKey::Named(n), Key::Named(k)) => n == k && self.shift == m.shift,
            (ChordKey::Char(c), _) => {
                let letter = c.chars().all(|c| c.is_ascii_alphabetic());
                if (letter || self.shift) && self.shift != m.shift {
                    return false;
                }
                let same = |k: &Key| {
                    matches!(k, Key::Character(s) if
                    if letter { s.eq_ignore_ascii_case(c) } else { s.as_str() == c })
                };
                // Windows delivers the UNSHIFTED printable while Ctrl/Alt/Super is
                // held (Character("/") for Ctrl+Shift+/), so a punctuation chord
                // spelled with its shifted symbol ("Ctrl+?") never sees that
                // character there (field, 2026-09-06: the shipped Ctrl+? could not
                // fire on any Windows box). When Shift is held with a modifier,
                // read the delivered base through the US layout's shifted pairs --
                // the one piece of layout knowledge this file carries, used only
                // on this path: a directly delivered "?" already matched above.
                let shifted_base = !letter && m.shift && (m.ctrl || m.alt || m.super_);
                if same(key) {
                    // Ctrl+Shift+/ on Windows arrives as "/": it was typed as "?",
                    // so the unshifted chord "Ctrl+/" must not claim it.
                    return !(shifted_base && !self.shift && us_shifted(c).is_some());
                }
                if shifted_base {
                    let as_shifted = |k: &Key| {
                        matches!(k, Key::Character(s) if us_shifted(s.as_str()) == Some(c.as_str()))
                    };
                    if as_shifted(key) || base.is_some_and(as_shifted) {
                        return true;
                    }
                }
                // Option may transform a letter on macOS. Explicit shifted base
                // punctuation (Ctrl+Shift+/) also uses the layout's unmodified key.
                // Never treat an unshifted '=' as '+' through this fallback.
                if (letter && (m.ctrl || m.alt || m.super_) || self.shift) && base.is_some_and(same) {
                    return true;
                }
                // macOS-only: Option also transforms many non-letter keys
                // (Option+= delivers "≠", base "="), so a symbol chord like
                // Alt+= can only fire there by reading the layout base too.
                // Gated to macOS: a US-International Linux layout delivers
                // Alt+AltGr+= as "×" (base "=", alt set, no shift) and must
                // keep typing "×", not fire Alt+= -- verified against
                // installed XKB data, not a hypothetical. `m.shift ==
                // self.shift` keeps the existing "Alt+= must not claim
                // Alt+Shift+=" guarantee.
                mac && !letter && m.alt && m.shift == self.shift && base.is_some_and(same)
            }
            _ => false,
        }
    }
    #[cfg(test)]
    fn matches(&self, key: &Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.matches_input(
            key,
            None,
            Modifiers {
                ctrl,
                alt,
                shift,
                super_: false,
            },
        )
    }
    fn label(&self, mac: bool) -> String {
        let mut parts = Vec::new();
        for (set, name, glyph) in [
            (self.ctrl, "Ctrl", "⌃"),
            (self.alt, "Alt", "⌥"),
            (self.shift, "Shift", "⇧"),
            (
                self.super_,
                if cfg!(windows) { "Win" } else { "Super" },
                "⌘",
            ),
        ] {
            if set {
                parts.push(if mac { glyph.into() } else { name.into() });
            }
        }
        parts.push(match &self.key {
            ChordKey::Char(s) => {
                if self.ctrl || self.alt || self.shift || self.super_ {
                    s.to_uppercase()
                } else {
                    s.clone()
                }
            }
            ChordKey::Named(n) => match n {
                NamedKey::ArrowUp => "↑".into(),
                NamedKey::ArrowDown => "↓".into(),
                NamedKey::ArrowLeft => "←".into(),
                NamedKey::ArrowRight => "→".into(),
                NamedKey::Escape => "Esc".into(),
                NamedKey::PageUp => "PgUp".into(),
                NamedKey::PageDown => "PgDn".into(),
                _ => format!("{n:?}"),
            },
        });
        parts.join(if mac { "" } else { "+" })
    }
}

#[derive(Debug, Clone)]
pub struct KeyBindings {
    chords: HashMap<Action, Vec<Chord>>,
}
impl KeyBindings {
    pub fn defaults() -> Self {
        let mut b = Self {
            chords: ACTIONS
                .iter()
                .map(|s| {
                    (
                        s.action,
                        s.defaults
                            .iter()
                            .map(|k| Chord::parse(k).expect("invalid default keybinding"))
                            .collect(),
                    )
                })
                .collect(),
        };
        // F11 fullscreen is claimed by macOS Mission Control ("Show
        // Desktop"); add the Mac convention as an extra chord rather than
        // replacing F11 (which still works when the owner has that Mission
        // Control shortcut off). Every other OS keeps F11 alone.
        if cfg!(target_os = "macos") {
            b.chords
                .get_mut(&Action::ToggleFullscreen)
                .expect("ToggleFullscreen is in ACTIONS")
                .push(Chord::parse("Ctrl+Super+f").expect("valid default keybinding"));
            // Finding 4, v0.6.5 macOS field report: Cmd+Shift+/ -- this
            // action's cross-platform "Primary+?" default, spelled out on
            // macOS -- is the SYSTEM'S OWN "Show Help menu" shortcut, on
            // by default; it never reaches the app at all, which is why
            // the field report's key log showed zero such events.
            // REPLACE the default here (never add to it) with the bare
            // Cmd+/, which nothing else on the system claims.
            b.chords.insert(
                Action::ToggleHelp,
                vec![Chord::parse("Cmd+/").expect("valid default keybinding")],
            );
        }
        b
    }
    pub fn load_layered() -> Self {
        let mut b = Self::defaults();
        if let Some(path) = find_config_file("SOT_KEYBINDINGS", "keybindings.toml") {
            match fs::read_to_string(&path) {
                Ok(text) => b.merge_text(&text),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "cannot read keybindings")
                }
            }
        }
        b
    }
    pub fn merge_text(&mut self, text: &str) {
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let Some(action) = Action::parse(name) else {
                tracing::warn!(line = i + 1, %name, "unknown keybinding action");
                continue;
            };
            let keys = parse_value(without_comment(value));
            let parsed: Option<Vec<_>> = keys.iter().map(|s| Chord::parse(s)).collect();
            match parsed {
                Some(chords) if !chords.is_empty() => {
                    self.chords.insert(action, chords);
                }
                _ => {
                    tracing::warn!(line = i + 1, %name, "invalid keybinding; keeping previous binding")
                }
            }
        }
    }
    /// Catalog order is dispatch precedence. Help filters with the same predicate.
    /// `literal_text`: the focused pane consumes typed characters (agent,
    /// terminal, Julia input, editor, prompts, Help search), so a chord that
    /// is just a printable key stays text there -- see `Chord::is_bare_text`.
    pub fn resolve(
        &self,
        key: &Key,
        base: Option<&Key>,
        m: Modifiers,
        literal_text: bool,
        allowed: impl Fn(Action) -> bool,
    ) -> Option<Action> {
        ACTIONS
            .iter()
            .find(|s| {
                allowed(s.action)
                    && self.chords[&s.action]
                        .iter()
                        .any(|c| !(literal_text && c.is_bare_text()) && c.matches_input(key, base, m))
            })
            .map(|s| s.action)
    }
    /// Only advertise chords this action actually wins in the current context.
    pub fn active_labels(&self, action: Action, literal_text: bool, allowed: impl Fn(Action) -> bool) -> Vec<String> {
        self.chords[&action]
            .iter()
            .filter(|c| {
                let key = match &c.key {
                    ChordKey::Char(s) => Key::Character(s.clone().into()),
                    ChordKey::Named(n) => Key::Named(*n),
                };
                self.resolve(
                    &key,
                    Some(&key),
                    Modifiers {
                        ctrl: c.ctrl,
                        alt: c.alt,
                        shift: c.shift,
                        super_: c.super_,
                    },
                    literal_text,
                    &allowed,
                ) == Some(action)
            })
            .map(|c| c.label(cfg!(target_os = "macos")))
            .collect()
    }
    pub fn labels(&self, action: Action) -> String {
        self.labels_for(action, cfg!(target_os = "macos"))
    }
    pub fn labels_for(&self, action: Action, mac: bool) -> String {
        self.chords[&action]
            .iter()
            .map(|c| c.label(mac))
            .collect::<Vec<_>>()
            .join(" / ")
    }
    pub fn first_label(&self, action: Action) -> String {
        self.chords[&action]
            .first()
            .map(|c| c.label(cfg!(target_os = "macos")))
            .unwrap_or_default()
    }
    #[cfg(test)]
    fn matches(&self, action: Action, key: &Key, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.chords[&action]
            .iter()
            .any(|c| c.matches(key, ctrl, alt, shift))
    }
}
fn named_key_from_str(s: &str) -> Option<NamedKey> {
    Some(match s {
        "Tab" => NamedKey::Tab,
        "Enter" | "Return" => NamedKey::Enter,
        "Escape" | "Esc" => NamedKey::Escape,
        "Space" => NamedKey::Space,
        "Backspace" => NamedKey::Backspace,
        "Insert" => NamedKey::Insert,
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

fn without_comment(value: &str) -> &str {
    let mut quote = None;
    for (i, c) in value.char_indices() {
        if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '#' && quote.is_none() {
            return &value[..i];
        }
    }
    value
}

/// Pull a value off the right of `key = ...`. Supports a single quoted
/// string (`"Alt+="`), a bare token (`Alt+=`), or a list (`["a", "b"]`).
/// Returns each chord string with its quotes stripped.
fn parse_value(v: &str) -> Vec<String> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut values = Vec::new();
        let mut quote = None;
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            if Some(c) == quote {
                quote = None;
            } else if quote.is_none() && matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c == ',' && quote.is_none() {
                values.push(strip_quotes(inner[start..i].trim()));
                start = i + 1;
            }
        }
        if !inner[start..].trim().is_empty() {
            values.push(strip_quotes(inner[start..].trim()));
        }
        values
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

#[cfg(test)]
#[path = "keybindings_tests.rs"]
mod tests;
