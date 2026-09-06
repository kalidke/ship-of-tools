//! Contextual help is a view of the dispatch catalog, never a second shortcut list.
use crate::keybindings::{Action, KeyBindings, Scope, ACTIONS};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Nav,
    Preview,
    Repl,
    Terminal,
    Monitor,
    Agent,
    Help,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Files,
    Modules,
    Sessions,
    Hosts,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Confirmation {
    #[default]
    None,
    Delete,
    Discard,
    Stale,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub pane: Pane,
    pub mode: Mode,
    pub file: Option<String>,
    pub image: bool,
    pub pages: bool,
    pub picker: bool,
    pub prompt: bool,
    pub editing: bool,
    pub workspace_locked: bool,
    pub editable: bool,
    pub confirmation: Confirmation,
    pub modal: bool,
    pub restore: bool,
    pub session: bool,
    pub alternate_screen: bool,
}
impl Context {
    pub fn allows(&self, action: Action) -> bool {
        use Scope::*;
        if self.confirmation != Confirmation::None {
            return match action.spec().scope {
                Global => true,
                Workspace => !self.editing && !self.workspace_locked,
                DeleteConfirm => self.confirmation == Confirmation::Delete,
                DiscardConfirm => self.confirmation == Confirmation::Discard,
                StaleEdit => self.confirmation == Confirmation::Stale,
                Modal => self.confirmation == Confirmation::Delete,
                _ => false,
            };
        }
        let nav = self.pane == Pane::Nav && !self.picker && !self.prompt;
        let preview = self.pane == Pane::Preview && !self.editing;
        let text = preview && !self.image;
        let repl = self.pane == Pane::Repl;
        let file = self.file.is_some();
        match action.spec().scope {
            Global => true,
            Workspace => !self.editing && !self.workspace_locked,
            Restore => self.restore && !self.modal,
            Scroll => !self.editing && (text || repl),
            Text => text,
            HalfScroll => text || repl,
            Picker => self.pane == Pane::Nav && self.picker,
            Nav => nav,
            FileNav => nav && file,
            Files => nav && self.mode == Mode::Files,
            FilesOrPicker => (nav && self.mode == Mode::Files) || (self.pane == Pane::Nav && self.picker),
            Navigation => nav || self.picker,
            Session => nav && self.mode == Mode::Sessions && self.session,
            File => (nav || preview) && file,
            Quarto => (nav || preview) && self.file.as_ref().is_some_and(|p| p.ends_with(".qmd")),
            Julia => nav && self.file.as_ref().is_some_and(|p| p.ends_with(".jl")),
            Repl => repl,
            Pty => repl || matches!(self.pane, Pane::Terminal | Pane::Agent),
            Llm => self.pane == Pane::Agent,
            Pages => preview && self.pages,
            Image => preview && self.image,
            Reading => preview || repl,
            PreviewFile => preview && self.editable,
            PageScroll => {
                (text && !self.pages)
                    || repl
                    || matches!(self.pane, Pane::Agent | Pane::Terminal) && !self.alternate_screen
            }
            Edit => self.editing,
            Modal => self.editing || self.picker || self.prompt,
            Prompt => self.prompt,
            Help => self.pane == Pane::Help,
            DeleteConfirm | DiscardConfirm | StaleEdit => false,
        }
    }
    pub fn title(&self) -> String {
        let pane = match self.pane {
            Pane::Nav => match self.mode {
                Mode::Files => "Files",
                Mode::Modules => "Modules",
                Mode::Sessions => "Sessions",
                Mode::Hosts => "Hosts",
            },
            Pane::Preview if self.editing => "Editor",
            Pane::Preview if self.pages => "Preview · pages",
            Pane::Preview if self.image => "Preview · image",
            Pane::Preview => "Preview · text",
            Pane::Repl => "Julia",
            Pane::Terminal => "Terminal",
            Pane::Monitor => "Monitor",
            Pane::Agent => "Agent",
            Pane::Help => "Help",
        };
        if self.picker {
            format!("{pane} · choose a folder")
        } else if self.prompt {
            format!("{pane} · input")
        } else {
            pane.into()
        }
    }
    pub fn note(&self) -> &'static str {
        match self.confirmation {
            Confirmation::Delete => return "Confirm deletion of the named file, or press another key to cancel.",
            Confirmation::Discard => return "Confirm discarding unsaved edits, or press another key to keep editing.",
            Confirmation::Stale => return "The file changed on disk. Reload it or keep editing before other editor commands are available.",
            Confirmation::None => {}
        }
        match self.pane {
            Pane::Agent | Pane::Terminal => "These are SoT controls. The application inside the terminal owns its other shortcuts.",
            Pane::Monitor => "Monitor displays host resource charts. Use drawer or focus shortcuts to return to work.",
            _ if self.editing => "Type to edit. Arrow keys move the cursor; Shift extends the selection. Save before closing to keep changes.",
            _ if self.prompt => "Type your value, confirm, or cancel. Navigation shortcuts are inactive while entering text.",
            _ => "Actions and shortcuts below use the current pane context and loaded keybindings.",
        }
    }
}

pub const PEEK_HOLD: Duration = Duration::from_secs(5);
pub const PEEK_FADE: Duration = Duration::from_millis(240);
#[derive(Debug, Clone)]
pub struct Peek {
    pub context: Context,
    pub started: Instant,
}
impl Peek {
    pub fn opacity(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed <= PEEK_HOLD {
            1.0
        } else {
            (1.0 - (elapsed - PEEK_HOLD).as_secs_f32() / PEEK_FADE.as_secs_f32()).clamp(0.0, 1.0)
        }
    }
}
#[derive(Default)]
pub struct Help {
    pub context: Context,
    pub query: String,
    pub selected: usize,
    pub all_panes: bool,
    pub peek: Option<Peek>,
}
impl Help {
    pub fn open(&mut self, context: Context) {
        self.context = context;
        self.query.clear();
        self.selected = 0;
        self.all_panes = false;
        self.peek = None;
    }
    pub fn actions(&self, bindings: &KeyBindings) -> Vec<Action> {
        let query = self.query.to_lowercase();
        let mut actions: Vec<_> = ACTIONS
            .iter()
            .filter(|s| self.all_panes || self.context.allows(s.action))
            .filter(|s| {
                query.split_whitespace().all(|word| {
                    format!(
                        "{} {} {} {} {}",
                        s.label,
                        s.detail,
                        s.name,
                        s.group,
                        bindings.labels(s.action)
                    )
                    .to_lowercase()
                    .contains(word)
                })
            })
            .map(|s| s.action)
            .collect();
        actions.sort_by_key(|a| {
            matches!(
                a.spec().scope,
                Scope::Global | Scope::Workspace | Scope::Restore
            )
        });
        actions
    }
    pub fn move_selection(&mut self, delta: isize, bindings: &KeyBindings) {
        let count = self.actions(bindings).len();
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
    }
    pub fn selected_action(&self, bindings: &KeyBindings) -> Option<Action> {
        self.actions(bindings).get(self.selected).copied()
    }
}

/// Fit complete hints. Keep the help entry even when the pane is narrow.
pub fn border(context: &Context, bindings: &KeyBindings, width: usize) -> String {
    if context.pane == Pane::Help {
        return truncate(
            &format!(
                "{} Return · {} Scope",
                bindings.first_label(Action::HelpClose),
                bindings.first_label(Action::HelpScope)
            ),
            width,
        );
    }
    let help = format!("{} Help", bindings.first_label(Action::ToggleHelp));
    let preferred: &[Action] = if context.picker {
        &[Action::SessionCreate, Action::SessionCreateCodex]
    } else if context.editing {
        &[Action::EditSave, Action::Cancel]
    } else {
        match context.pane {
            Pane::Preview if context.image => &[Action::PreviewPngZoomIn, Action::PreviewPngReset],
            Pane::Preview if context.pages => &[Action::PageNext, Action::PagePrev],
            Pane::Preview => &[Action::EditFile, Action::CopyCode],
            Pane::Nav if context.file.as_ref().is_some_and(|p| p.ends_with(".jl")) => {
                &[Action::RunCurrent, Action::RunFresh]
            }
            Pane::Nav => &[Action::NavExpand, Action::NavOpen],
            Pane::Repl => &[Action::ReplSubmit, Action::ReplClear],
            Pane::Agent => &[Action::Paste, Action::CopySelection],
            _ => &[],
        }
    };
    let mut hints = Vec::new();
    for &a in preferred.iter().filter(|a| context.allows(**a)) {
        let keys = bindings.active_labels(a, |a| context.allows(a));
        let Some(key) = keys.first() else {
            continue;
        };
        let hint = format!("{} {}", key, a.spec().label);
        let candidate = format!(
            "{} · {}",
            hints
                .iter()
                .chain(std::iter::once(&hint))
                .cloned()
                .collect::<Vec<_>>()
                .join(" · "),
            help
        );
        if unicode_width::UnicodeWidthStr::width(candidate.as_str()) <= width {
            hints.push(hint);
        }
    }
    hints.push(help);
    truncate(&hints.join(" · "), width)
}
pub fn truncate(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The transient list contains this pane's actions. Global controls remain in
/// the drawer so the short overlay stays readable at normal pane widths.
pub fn peek_lines(context: &Context, bindings: &KeyBindings) -> Vec<String> {
    let mut lines = vec![format!("{} · actions", context.title()), String::new()];
    for spec in ACTIONS.iter().filter(|s| {
        context.allows(s.action)
            && !matches!(s.scope, Scope::Global | Scope::Workspace | Scope::Restore)
    }) {
        let keys = bindings.active_labels(spec.action, |a| context.allows(a));
        if !keys.is_empty() {
            lines.push(format!("{}  ·  {}", keys.join(" / "), spec.label));
        }
    }
    if lines.len() == 2 {
        lines.push(context.note().into());
    }
    lines.push(String::new());
    lines.push(format!(
        "{} again: drawer · Esc close",
        bindings.first_label(Action::ToggleHelp)
    ));
    lines
}

pub fn render(frame: &mut Frame<'_>, rect: Rect, help: &Help, bindings: &KeyBindings) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    let width = rect.width as usize;
    let actions = help.actions(bindings);
    let selected = help.selected.min(actions.len().saturating_sub(1));
    let detail_rows = if rect.height >= 11 { 4 } else { 0 };
    let list_rows = (rect.height as usize)
        .saturating_sub(3 + detail_rows)
        .max(1);
    let first = selected.saturating_sub(list_rows - 1);
    let mut lines = vec![
        Line::from(Span::styled(
            truncate(
                &format!(
                    "About: {}   ·   {}",
                    help.context.title(),
                    if help.all_panes {
                        "All panes"
                    } else {
                        "This pane"
                    }
                ),
                width,
            ),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(truncate(&format!("Search: {}▏", help.query), width)),
    ];
    for (i, a) in actions.iter().enumerate().skip(first).take(list_rows) {
        let s = a.spec();
        let keys = bindings.active_labels(*a, |a| help.context.allows(a));
        let key_label = if help.all_panes && !help.context.allows(*a) {
            bindings.labels(*a)
        } else if keys.is_empty() {
            "binding shadowed".into()
        } else {
            keys.join(" / ")
        };
        let label = format!(
            "{} {}  ·  {}",
            if i == selected { "›" } else { " " },
            s.label,
            key_label
        );
        lines.push(Line::from(Span::styled(
            truncate(&label, width),
            if i == selected {
                Style::default()
                    .fg(Color::LightCyan)
                    .bg(Color::Rgb(22, 48, 65))
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
    }
    if actions.is_empty() {
        lines.push(Line::from("No matching actions."));
    }
    let list_rect = Rect {
        height: rect.height.saturating_sub(detail_rows as u16 + 1),
        ..rect
    };
    frame.render_widget(Paragraph::new(lines), list_rect);
    if detail_rows > 0 {
        let a = actions.get(selected);
        let detail = a
            .map(|a| {
                format!(
                    "{} · {}\n{}\n{}",
                    a.spec().group,
                    a.spec().name,
                    a.spec().detail,
                    if help.all_panes && !help.context.allows(*a) {
                        "Available in another pane or context."
                    } else {
                        help.context.note()
                    }
                )
            })
            .unwrap_or_else(|| help.context.note().into());
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::Rgb(167, 194, 212))),
            Rect {
                y: rect.y + rect.height - detail_rows as u16 - 1,
                height: detail_rows as u16,
                ..rect
            },
        );
    }
    frame.render_widget(
        Paragraph::new(truncate(
            &format!(
                "{} Select · {} Scope · {} Manual · {} Return",
                bindings.first_label(Action::HelpDown),
                bindings.first_label(Action::HelpScope),
                bindings.first_label(Action::HelpManual),
                bindings.first_label(Action::HelpClose)
            ),
            width,
        ))
        .style(Style::default().fg(Color::DarkGray)),
        Rect {
            y: rect.y + rect.height - 1,
            height: 1,
            ..rect
        },
    );
}

pub fn manual_url(action: Action) -> &'static str {
    match action.spec().group {
        "Image" | "Text" | "Pages" => "https://kalidke.github.io/ship-of-tools/dev/guide/previews/",
        "Julia" => "https://kalidke.github.io/ship-of-tools/dev/guide/repl/",
        "Navigation" | "Files" | "Sessions" | "Picker" => {
            "https://kalidke.github.io/ship-of-tools/dev/guide/modes/"
        }
        _ => "https://kalidke.github.io/ship-of-tools/dev/ref/keybindings/",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fade_holds_for_five_seconds_and_expires() {
        let start = Instant::now();
        let p = Peek {
            started: start,
            context: Context::default(),
        };
        assert_eq!(p.opacity(start + PEEK_HOLD), 1.0);
        assert!((p.opacity(start + PEEK_HOLD + PEEK_FADE / 2) - 0.5).abs() < 0.01);
        assert_eq!(p.opacity(start + PEEK_HOLD + PEEK_FADE), 0.0);
    }
    #[test]
    fn pane_context_controls_actions_and_binding_search() {
        let mut b = KeyBindings::defaults();
        b.merge_text("preview.png.zoom_in = \"Cmd+z\"");
        let c = Context {
            pane: Pane::Preview,
            image: true,
            ..Context::default()
        };
        assert!(c.allows(Action::PreviewPngZoomIn));
        assert!(!c.allows(Action::RunFresh));
        assert!(!c.allows(Action::CopyCode));
        let mut h = Help::default();
        h.open(c.clone());
        h.query = "zoom".into();
        assert!(h.actions(&b).contains(&Action::PreviewPngZoomIn));
        assert_eq!(h.context, c); // searching doesn't replace the origin context
        assert!(border(&c, &b, 100).contains(&b.first_label(Action::PreviewPngZoomIn)));
    }
    /// A hidden folder (e.g. a Julia depot) is a legitimate workspace root,
    /// so the picker must offer the same hidden toggle Files mode has --
    /// and only Files mode among the nav modes.
    #[test]
    fn hidden_toggle_is_allowed_in_files_nav_and_in_the_picker_only() {
        let files = Context { pane: Pane::Nav, mode: Mode::Files, ..Context::default() };
        assert!(files.allows(Action::ToggleHidden));
        let picker = Context { pane: Pane::Nav, mode: Mode::Sessions, picker: true, ..Context::default() };
        assert!(picker.allows(Action::ToggleHidden));
        let modules = Context { pane: Pane::Nav, mode: Mode::Modules, ..Context::default() };
        assert!(!modules.allows(Action::ToggleHidden));
        let preview = Context { pane: Pane::Preview, ..Context::default() };
        assert!(!preview.allows(Action::ToggleHidden));
    }

    #[test]
    fn input_and_pagination_suppress_unrelated_actions() {
        let c = Context {
            pane: Pane::Preview,
            image: true,
            pages: true,
            ..Context::default()
        };
        assert!(c.allows(Action::PageNext));
        assert!(!c.allows(Action::ScrollPageDown));
        let c = Context {
            pane: Pane::Nav,
            picker: true,
            ..Context::default()
        };
        assert!(c.allows(Action::SessionCreate));
        assert!(!c.allows(Action::Quit));
    }
    #[test]
    fn border_keeps_help_and_never_overflows() {
        let b = KeyBindings::defaults();
        for width in 0..100 {
            let s = border(&Context::default(), &b, width);
            assert!(unicode_width::UnicodeWidthStr::width(s.as_str()) <= width);
        }
        // The label is OS-rendered (glyphs on macOS), so derive the expectation.
        assert_eq!(
            border(&Context::default(), &b, 12),
            format!("{} Help", b.first_label(Action::ToggleHelp))
        );
    }
    #[test]
    fn confirmation_help_exposes_only_the_active_decision() {
        let mut c = Context {
            pane: Pane::Nav,
            prompt: true,
            confirmation: Confirmation::Delete,
            ..Default::default()
        };
        assert!(c.allows(Action::DeleteConfirm));
        assert!(!c.allows(Action::Confirm)); // Enter cancels deletion; it must not say confirm.
        assert!(!c.allows(Action::NavOpen));
        c.pane = Pane::Preview;
        c.editing = true;
        c.confirmation = Confirmation::Discard;
        assert!(c.allows(Action::DiscardConfirm));
        assert!(!c.allows(Action::Cancel)); // Escape now means discard, not cancel.
        assert!(!c.allows(Action::EditSave));
        c.confirmation = Confirmation::Stale;
        assert!(c.allows(Action::StaleReload));
        assert!(c.allows(Action::StaleKeep));
        assert!(!c.allows(Action::EditSave));
        assert!(!c.allows(Action::DiscardConfirm));
    }

    #[test]
    fn annotation_editing_and_terminal_paging_match_the_active_view() {
        let c = Context {
            pane: Pane::Preview,
            editable: true,
            ..Default::default()
        };
        assert!(
            c.allows(Action::EditFile),
            "Annotations need not have a files-mode path"
        );
        for pane in [Pane::Agent, Pane::Terminal] {
            let mut c = Context {
                pane,
                ..Default::default()
            };
            assert!(c.allows(Action::ScrollPageUp));
            c.alternate_screen = true;
            assert!(!c.allows(Action::ScrollPageUp));
        }
    }
}
