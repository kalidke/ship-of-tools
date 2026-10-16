//! Which window a command sent with no window named goes to: what a session puts in the preview pane (a preview or a
//! reveal) to every window, and everything else, an open in the OS browser (`docs`, `open-url`, a `wglshow` page)
//! included, to one window, the window in use.

use super::{ActiveFrontend, ClientInfo, ClientsSnapshot};

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
    pub fn in_use(&self) -> Option<(ActiveFrontend, &'static str)> {
        resolve_in_use(&self.clients)
    }
}

/// `in_use`'s rule: one ordering, highest first: (last used, connection serial).
fn resolve_in_use(clients: &[ClientInfo]) -> Option<(ActiveFrontend, &'static str)> {
    let (c, handle) = clients
        .iter()
        .filter(|c| c.role == "fe")
        .filter_map(|c| Some((c, c.name.as_deref().filter(|n| !n.is_empty())?)))
        .max_by_key(|(c, _)| (c.last_person_input_at, c.serial))?;
    let why = if c.last_person_input_at.is_some() { "last-used" } else { "last-connected" };
    Some((ActiveFrontend { serial: c.serial, handle: handle.to_string() }, why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::Clients;
    use std::time::{Duration, Instant};

    fn fe(serial: u64, name: &str, used: Option<Instant>) -> ClientInfo {
        ClientInfo {
            serial,
            client_id: format!("c{serial}"),
            app_version: "0.6.6".into(),
            protocol: 1,
            host: None,
            role: "fe".into(),
            instance: None,
            name: Some(name.into()),
            last_person_input_at: used,
            sessions: None,
        }
    }

    fn chosen(clients: &[ClientInfo]) -> Option<(String, &'static str)> {
        resolve_in_use(clients).map(|(to, why)| (to.handle, why))
    }

    fn pick(handle: &str, why: &'static str) -> Option<(String, &'static str)> {
        Some((handle.to_string(), why))
    }

    #[test]
    fn the_window_in_use_is_the_one_with_the_latest_input_the_one_marked_active() {
        let now = Instant::now() + Duration::from_secs(3600);
        let s = |secs| Some(now - Duration::from_secs(secs));
        let clients = [fe(1, "fe@a", s(180)), fe(2, "fe@b", s(5)), fe(3, "fe@c", None)];
        assert_eq!(chosen(&clients), pick("fe@b", "last-used"), "the latest input, not the window connected last");
        let active = Clients::resolve_active(&clients, now).map(|a| a.handle);
        assert_eq!(active.as_deref(), Some("fe@b"), "the window `sot-fe version` marks active");
    }

    #[test]
    fn with_no_window_active_the_one_used_last_and_with_none_used_the_one_connected_last() {
        let now = Instant::now() + Duration::from_secs(3600);
        let s = |secs| Some(now - Duration::from_secs(secs));
        let idle = [fe(1, "fe@a", s(1200)), fe(2, "fe@b", s(600)), fe(3, "fe@c", None)];
        assert_eq!(Clients::resolve_active(&idle, now), None, "no window used within five minutes");
        assert_eq!(chosen(&idle), pick("fe@b", "last-used"), "the window used last, however long ago");
        let never = [fe(1, "fe@a", None), fe(2, "fe@b", None)];
        assert_eq!(chosen(&never), pick("fe@b", "last-connected"), "no window ever used");
        let mut others = [fe(1, "fe@a", None), fe(2, "", None), fe(3, "fe@c", None)];
        others[2].role = "agent".into();
        assert_eq!(chosen(&others), pick("fe@a", "last-connected"), "only named frontends are chosen");
        others[0].name = None;
        assert_eq!(chosen(&others), None, "no window attached: none");
    }

    #[test]
    fn a_preview_goes_to_every_window_and_every_other_command_to_one() {
        let clients = Clients::new();
        let a = clients.register("a", "0.6.6", 1, "fe".into(), None, None, Some("fe@a".into()));
        let _b = clients.register("b", "0.6.6", 1, "fe".into(), None, None, Some("fe@b".into()));
        clients.touch_person_input(a.serial());
        let snap = clients.snapshot_with_active();
        for cmd in ["preview", "reveal"] {
            assert_eq!(snap.window_for(cmd), None, "{cmd}: every window");
        }
        for cmd in ["docs", "open_url", "notify", "goto_workspace", "goto_mode", "relaunch"] {
            assert_eq!(snap.window_for(cmd).map(|(to, _)| to.serial), Some(a.serial()), "{cmd}: the window in use");
        }
    }
}
