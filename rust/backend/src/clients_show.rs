//! Which window a command sent with no window named goes to: what a session puts in the preview pane (a preview or a
//! reveal) to every window, and everything else, an open in the OS browser (`docs`, `open-url`, a `wglshow` page)
//! included, to one window, the window in use, or to none when the window a person used last is away.

use super::{ActiveFrontend, ClientInfo, ClientsSnapshot};

/// The commands that put something in the preview pane: every window gets them, so none is chosen.
const PANE: &[&str] = &["preview", "reveal"];

/// Where an untargeted `fe.command` goes (`window_for`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Every window: a preview-pane command.
    Every,
    /// One window, by serial, and why it was chosen.
    One(ActiveFrontend, &'static str),
    /// No window: the window a person used last is away, on the computer named, or no named window is attached, or a
    /// relaunch has no active frontend (`None`). Nothing is sent to another window.
    Nowhere(Option<String>),
}

impl ClientsSnapshot {
    /// Where an untargeted `fe.command` `cmd` goes (`handle_fe_command_send`). `relaunch` goes to the active frontend
    /// alone (`used`; nowhere when no window was used within `ACTIVE_WINDOW`); a preview-pane command (`PANE`) to every
    /// window; any other command, a browser open included, to `in_use`, or nowhere when it has none.
    pub fn window_for(&self, cmd: &str) -> Route {
        match cmd {
            "relaunch" => self.active().cloned().map_or(Route::Nowhere(None), |to| Route::One(to, "used")),
            _ if PANE.contains(&cmd) => Route::Every,
            _ => match self.in_use() {
                Ok((to, why)) => Route::One(to, why),
                Err(away) => Route::Nowhere(away),
            },
        }
    }

    /// The window in use, and why, or the computer whose window it was when it is away. The window a person used most
    /// recently, live or away, ranks first: live, it is `last-used` (within `ACTIVE_WINDOW` that is the active frontend
    /// `sot-fe version` marks); away (asleep, closed, redialing), a live window on its computer is chosen
    /// (`same-computer`, as a relaunched window is), else none and `Err` names that computer, so nothing opens on a
    /// computer the person left. With no window ever used, `last-connected`, the live window that connected last.
    /// `Err(None)` only when no named frontend is attached: a browser open never goes to every window. One ordering,
    /// highest first: (last used, connection serial).
    pub fn in_use(&self) -> Result<(ActiveFrontend, &'static str), Option<String>> {
        let windows = || {
            let fe = self.clients.iter().filter(|c| c.role == "fe");
            fe.filter_map(|c| Some((c, c.name.as_deref().filter(|n| !n.is_empty())?)))
        };
        let to = |(c, handle): (&ClientInfo, &str)| ActiveFrontend { serial: c.serial, handle: handle.to_string() };
        let live = windows().max_by_key(|(c, _)| (c.last_person_input_at, c.serial));
        if let Some((host, at)) = &self.away {
            // A tie goes to the window away: nothing opens on another computer.
            if live.is_none_or(|(c, _)| c.last_person_input_at <= Some(*at)) {
                let here = windows().filter(|(c, _)| c.host.as_deref() == Some(host.as_str()));
                let here = here.max_by_key(|(c, _)| c.serial);
                return here.map(|w| (to(w), "same-computer")).ok_or_else(|| Some(host.clone()));
            }
        }
        let (c, handle) = live.ok_or(None)?;
        let why = if c.last_person_input_at.is_some() { "last-used" } else { "last-connected" };
        Ok((to((c, handle)), why))
    }
}

#[cfg(test)]
#[path = "clients_show_tests.rs"]
mod tests;
