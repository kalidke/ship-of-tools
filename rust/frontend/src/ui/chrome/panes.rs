//! Pane focus and the slots it names: `PaneFocus`, `DrawerContent`, `SpatialDir`, the cached pane rects.

use super::*;

/// Which of the four quadrant panes has keyboard focus. Spatial moves
/// via Ctrl+Arrow. Tab is deliberately not a focus switcher — it must
/// reach the terminal panes for shell/REPL completion. Status-line and
/// the panel borders signal which is active.
///
/// Pane semantics:
///   - NavTree consumes character keys as mode/nav shortcuts.
///   - Repl consumes character keys as code to evaluate.
///   - Preview / Llm are passive today (focused for visual indication and
///     so Ctrl+Arrow has a four-corner home); scroll/interaction lands
///     when those panes grow real affordances.
///
/// User-configurable layout + keybindings live in a settings file the LLM
/// can edit — TODO. For now the names + Ctrl+Arrow adjacency are coded
/// directly so the structure stays obvious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum PaneFocus {
    NavTree,
    Preview,
    Llm,
    Repl,
}

impl PaneFocus {
    fn slot(self) -> crate::settings::Slot {
        use crate::settings::Slot;
        match self {
            PaneFocus::NavTree => Slot::Nav,
            PaneFocus::Preview => Slot::Preview,
            PaneFocus::Llm => Slot::Llm,
            PaneFocus::Repl => Slot::Repl,
        }
    }

    fn from_slot(slot: crate::settings::Slot) -> Self {
        use crate::settings::Slot;
        match slot {
            Slot::Nav => PaneFocus::NavTree,
            Slot::Preview => PaneFocus::Preview,
            Slot::Llm => PaneFocus::Llm,
            Slot::Repl => PaneFocus::Repl,
        }
    }

    /// Spatial neighbour of `self` in `dir`, walking only the panes the
    /// layout shows: `columns` is the preset's column order (ADR 0014) and
    /// `drawer` is the slot the open drawer shows (`None` when closed or the
    /// preset has none). Left/Right step along `columns` and stop at its
    /// ends; Down enters the drawer; Up from a column stays. The drawer pane
    /// is not a column: Up leaves it for the preview (else the first
    /// column), Left/Right for the first/last column. A `self` the layout
    /// hides lands on the first column.
    pub(in crate::ui) fn move_in(
        self,
        dir: SpatialDir,
        columns: &[crate::settings::Slot],
        drawer: Option<crate::settings::Slot>,
    ) -> Self {
        use crate::settings::Slot;
        use SpatialDir::*;
        let (Some(&first), Some(&last)) = (columns.first(), columns.last()) else {
            return self;
        };
        let Some(i) = columns.iter().position(|&c| c == self.slot()) else {
            if drawer != Some(self.slot()) {
                return Self::from_slot(first);
            }
            return Self::from_slot(match dir {
                Up if columns.contains(&Slot::Preview) => Slot::Preview,
                Right => last,
                Down => return self,
                _ => first,
            });
        };
        match dir {
            Left => Self::from_slot(columns[i.saturating_sub(1)]),
            Right => Self::from_slot(columns[(i + 1).min(columns.len() - 1)]),
            Down => drawer.map_or(self, Self::from_slot),
            Up => self,
        }
    }
}

/// What the bottom drawer is showing. `Closed` = hidden (three columns
/// occupy the full window height); `Repl` = the Julia REPL (Ctrl+J);
/// `Terminal` = the local PTY terminal (Ctrl+T). `layout::compute` only
/// cares whether the drawer is open (`!= Closed`); the variant selects
/// which content renders into the drawer rect and the title label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum DrawerContent {
    Closed,
    Repl,
    Terminal,
    Monitor,
    Help,
}

impl DrawerContent {
    /// True when the drawer occupies vertical space (anything but `Closed`).
    pub(in crate::ui) fn is_open(self) -> bool {
        self != DrawerContent::Closed
    }

    /// Toggle `slot` per the symmetric Ctrl+J / Ctrl+T rule: pressing a
    /// drawer key opens its content, swaps to it if the other is showing,
    /// and closes if its own content is already showing.
    pub(in crate::ui) fn toggle(self, slot: DrawerContent) -> DrawerContent {
        if self == slot {
            DrawerContent::Closed
        } else {
            slot
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::ui) enum SpatialDir {
    Up,
    Down,
    Left,
    Right,
}

/// The slot maximization gives the whole area, if any. While a leave has a
/// line to show, none: the nav pane draws that line (`nav_pinned_rows`).
pub(in crate::ui) fn maximize_slot(maximized: bool, focus: PaneFocus, leave_line: bool) -> Option<crate::settings::Slot> {
    (maximized && !leave_line).then(|| focus.slot())
}

/// Cached pane geometry. `tl` and `bl` are reserved for future use
/// (click-to-focus, per-pane mouse interactions); only `tr` / `br` are
/// read today for keyboard viewport sizing.
#[derive(Default, Clone, Copy, Debug)]
#[allow(dead_code)]
pub(in crate::ui) struct PaneRects {
    pub(in crate::ui) nav: ratatui::layout::Rect,
    pub(in crate::ui) preview: ratatui::layout::Rect,
    pub(in crate::ui) llm: ratatui::layout::Rect,
    /// Drawer (REPL) rect when the drawer is open; zero-area when
    /// closed. Consumers using `.repl.height` already handle zero
    /// safely (clamp to 1 / no-op).
    pub(in crate::ui) repl: ratatui::layout::Rect,
}

impl State {
    /// The one write of `focus`: moving it off the navigation pane dismisses
    /// the quit prompt (`quit_prompt_on_focus`), so the prompt is on screen
    /// only while the tree has focus.
    pub(in crate::ui) fn set_focus(&mut self, to: PaneFocus) {
        if quit_prompt_on_focus(self.nav_prompt.as_ref(), to) == Some(QuitPromptStep::Cancel) {
            self.cancel_nav_prompt();
        }
        self.focus = to;
    }

    /// Dismiss the active NavTree prompt without acting. The cancel message is
    /// variant-aware so the user sees which prompt they backed out of.
    pub(in crate::ui) fn cancel_nav_prompt(&mut self) {
        self.status = match self.nav_prompt {
            Some(NavPrompt::ConfirmDelete { .. }) => "delete · cancelled".to_string(),
            Some(NavPrompt::ConfirmQuit { .. }) => "quit · cancelled".to_string(),
            Some(NavPrompt::ScaleEntry { .. }) => "pixel size · cancelled".to_string(),
            _ => "new file · cancelled".to_string(),
        };
        self.nav_prompt = None;
        // Cancelling a scale prompt returns you where you were (see
        // `scale_entry_prior_focus`). Only fires for that prompt; the other
        // variants are opened FROM the tree and never moved focus.
        if let Some(prior) = self.scale_entry_prior_focus.take() {
            self.set_focus(prior);
        }
        self.window.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_in_walks_only_the_laid_out_columns() {
        use crate::settings::Slot as S;
        use PaneFocus::*;
        use SpatialDir::*;
        let full = [S::Nav, S::Preview, S::Llm];
        assert_eq!(NavTree.move_in(Right, &full, None), Preview);
        assert_eq!(Repl.move_in(Up, &full, Some(S::Repl)), Preview);
        assert_eq!(Repl.move_in(Left, &full, Some(S::Repl)), NavTree);
        assert_eq!(Repl.move_in(Right, &full, Some(S::Repl)), Llm);
        let portrait = [S::Nav, S::Preview];
        assert_eq!(Preview.move_in(Right, &portrait, None), Preview);
        assert_eq!(Repl.move_in(Right, &portrait, Some(S::Repl)), Preview);
        assert_eq!(Preview.move_in(Down, &portrait, None), Preview);
        assert_eq!(Preview.move_in(Down, &portrait, Some(S::Repl)), Repl);
        assert_eq!(Llm.move_in(Left, &portrait, None), NavTree);
        let no_preview = [S::Nav, S::Llm];
        assert_eq!(NavTree.move_in(Right, &no_preview, None), Llm);
        assert_eq!(Llm.move_in(Left, &no_preview, None), NavTree);
        assert_eq!(Repl.move_in(Up, &no_preview, Some(S::Repl)), NavTree);
        assert_eq!(Repl.move_in(Up, &full, None), NavTree);
        let custom = [S::Nav, S::Llm];
        assert_eq!(NavTree.move_in(Down, &custom, Some(S::Preview)), Preview);
        assert_eq!(Preview.move_in(Up, &custom, Some(S::Preview)), NavTree);
        assert_eq!(Preview.move_in(Right, &custom, Some(S::Preview)), Llm);
        assert_eq!(Preview.move_in(Down, &custom, Some(S::Preview)), Preview);
        assert_eq!(Repl.move_in(Down, &custom, Some(S::Preview)), NavTree);
    }

    #[test]
    fn drawer_toggle_is_symmetric() {
        use DrawerContent::*;
        // Open from closed.
        assert_eq!(Closed.toggle(Repl), Repl);
        assert_eq!(Closed.toggle(Terminal), Terminal);
        // Same key closes.
        assert_eq!(Repl.toggle(Repl), Closed);
        assert_eq!(Terminal.toggle(Terminal), Closed);
        // Other key swaps content without closing.
        assert_eq!(Repl.toggle(Terminal), Terminal);
        assert_eq!(Terminal.toggle(Repl), Repl);
    }

    #[test]
    fn drawer_is_open_only_when_not_closed() {
        assert!(!DrawerContent::Closed.is_open());
        assert!(DrawerContent::Repl.is_open());
        assert!(DrawerContent::Terminal.is_open());
    }
}
