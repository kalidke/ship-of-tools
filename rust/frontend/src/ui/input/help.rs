//! Contextual help is a view of the dispatch catalog, never a second shortcut list.
use crate::ui::nav::Mode;
use crate::ui::input::keybindings::{Action, ActionSpec, KeyBindings, Scope, ACTIONS};
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
            Quit => !self.editing && !self.picker && !self.prompt && !matches!(self.pane, Pane::Agent | Pane::Terminal),
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
    /// Does the focused pane consume typed characters? Agent, terminal and
    /// Julia input, the editor, prompts, and Help's own search field. A chord
    /// that is just a printable key never dispatches here
    /// (`KeyBindings::resolve`'s `literal_text`).
    pub fn consumes_text(&self) -> bool {
        self.editing
            || self.prompt
            || matches!(self.pane, Pane::Agent | Pane::Terminal | Pane::Repl | Pane::Help)
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
        let keys = bindings.active_labels(a, context.consumes_text(), |a| context.allows(a));
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
        let keys = bindings.active_labels(spec.action, context.consumes_text(), |a| context.allows(a));
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

#[allow(clippy::too_many_lines, reason = "renders the Help drawer; predates the 100-line limit")]
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
        let keys = bindings.active_labels(*a, help.context.consumes_text(), |a| help.context.allows(a));
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
                "{} Select · {} Scope · {} Manual · {} Cheat sheet · {} Return",
                bindings.first_label(Action::HelpDown),
                bindings.first_label(Action::HelpScope),
                bindings.first_label(Action::HelpManual),
                bindings.first_label(Action::HelpCheatSheet),
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

/// Escape the five HTML-special characters. No new crate for five `match` arms.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A standalone, printable page of every listed action and its loaded
/// shortcuts (user overrides included, via `bindings.labels`), one table
/// per Help group in table-definition order. No fs source for this page
/// -- it's synthesized fresh from `bindings` -- so the caller opens it
/// the same way the Quarto quick-render and sourceless-preview `o` do:
/// write it to a temp file and hand that to the OS browser, whose own
/// Print / Save-as-PDF is the export path (no PDF library here).
pub fn cheat_sheet_html(bindings: &KeyBindings) -> String {
    let mut groups: Vec<(&'static str, Vec<&'static ActionSpec>)> = Vec::new();
    for spec in ACTIONS.iter() {
        match groups.iter_mut().find(|(group, _)| *group == spec.group) {
            Some((_, specs)) => specs.push(spec),
            None => groups.push((spec.group, vec![spec])),
        }
    }

    let mut tables = String::new();
    for (group, specs) in &groups {
        tables.push_str(&format!("<h2>{}</h2>\n<table>\n", html_escape(group)));
        tables.push_str("<tr><th>Shortcut(s)</th><th>Action</th><th>What it does</th></tr>\n");
        for spec in specs {
            // `id` carries the dotted action name -- not shown, but a
            // find-in-page/deep-link hook, and how the test below tells
            // one row from another without depending on label wording.
            tables.push_str(&format!(
                "<tr id=\"{}\"><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                spec.name,
                html_escape(&bindings.labels(spec.action)),
                html_escape(spec.label),
                html_escape(spec.detail),
            ));
        }
        tables.push_str("</table>\n");
    }

    format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\">\n\
         <title>Ship of Tools keybindings</title>\n\
         <style>\n\
         body {{ font-family: system-ui, sans-serif; font-size: 13px; margin: 16px; columns: 2; column-gap: 24px; }}\n\
         h2 {{ column-span: none; font-size: 14px; margin: 14px 0 4px; }}\n\
         table {{ border-collapse: collapse; width: 100%; margin-bottom: 8px; }}\n\
         td, th {{ padding: 2px 6px; text-align: left; border-bottom: 1px solid #ccc; font-size: 12px; }}\n\
         @media print {{ @page {{ margin: 12mm }} h2 {{ page-break-after: avoid }} tr {{ page-break-inside: avoid }} }}\n\
         </style></head><body>\n\
         <p>Ship of Tools v{version} keybindings &mdash; {date}</p>\n\
         {tables}\
         </body></html>\n",
        version = env!("CARGO_PKG_VERSION"),
        date = chrono::Local::now().format("%Y-%m-%d"),
        tables = tables,
    )
}

#[cfg(test)]
#[path = "help_tests.rs"]
mod tests;
