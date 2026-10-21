//! Which window a command sent with no window named goes to: what a session puts in the preview pane (a preview or a
//! reveal) to every window, and everything else, an open in the OS browser (`docs`, `open-url`, a `wglshow` page)
//! included, to one window, the window in use.

use super::{ActiveFrontend, ClientsSnapshot};

/// The commands that put something in the preview pane: every window gets them, so none is chosen.
const PANE: &[&str] = &["preview", "reveal"];

impl ClientsSnapshot {
    /// The one window an untargeted `fe.command` `cmd` goes to, and why (`handle_fe_command_send`); `None` sends it to
    /// every window. `relaunch` goes to the active frontend alone (`used`; none when no window was used within
    /// `ACTIVE_WINDOW`, and then it goes nowhere); a preview-pane command (`PANE`) to every window; any other command,
    /// a browser open included, to `in_use`.
    pub fn window_for(&self, cmd: &str) -> Option<(ActiveFrontend, &'static str)> {
        match cmd {
            "relaunch" => self.active().cloned().map(|to| (to, "used")),
            _ if PANE.contains(&cmd) => None,
            _ => self.in_use(),
        }
    }

    /// The window in use, and why: `last-used`, the window a person used most recently, however long ago (within
    /// `ACTIVE_WINDOW` that is the active frontend `sot-fe version` marks); otherwise `last-connected`, the window
    /// that connected last. `None` only when no named frontend is attached: a browser open never goes to every window.
    /// One ordering, highest first: (last used, connection serial).
    pub fn in_use(&self) -> Option<(ActiveFrontend, &'static str)> {
        let (c, handle) = self
            .clients
            .iter()
            .filter(|c| c.role == "fe")
            .filter_map(|c| Some((c, c.name.as_deref().filter(|n| !n.is_empty())?)))
            .max_by_key(|(c, _)| (c.last_person_input_at, c.serial))?;
        let why = if c.last_person_input_at.is_some() { "last-used" } else { "last-connected" };
        Some((ActiveFrontend { serial: c.serial, handle: handle.to_string() }, why))
    }
}

#[cfg(test)]
#[path = "clients_show_tests.rs"]
mod tests;
