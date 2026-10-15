//! Which window a show goes to: the roster's one answer for a preview, a reveal, `docs`, `open-url` or a `wglshow` page
//! sent with no window named, and for every other untargeted `fe.command` but `relaunch`.

use super::{ActiveFrontend, ClientInfo, Clients, ClientsSnapshot, ACTIVE_WINDOW};
use std::time::Instant;

impl Clients {
    /// Record the row the window on connection `serial` now shows (`workspace.activate`, `server/dispatch.rs`), on
    /// the connection and on its window. A no-op for an unregistered serial.
    pub fn show_row(&self, serial: u64, row: String) {
        self.inner.lock().unwrap().report(serial, |_, shown| *shown = Some(row.clone()));
    }
}

/// The commands that show something from a row, whose `workspace` argument names the row: they go to a window on it.
const SHOWS: &[&str] = &["preview", "reveal", "docs", "open_url"];

impl ClientsSnapshot {
    /// The window an untargeted `fe.command` `cmd` goes to, and why (`handle_fe_command_send`): `relaunch` to the
    /// active frontend alone (`used`, none when no window was used within `ACTIVE_WINDOW`); a show (`SHOWS`) to
    /// `show_target` of the row its `workspace` argument names; any other command to `show_target` with no row.
    pub fn window_for(
        &self,
        cmd: &str,
        args: &serde_json::Value,
        workspaces: &crate::rows::Workspaces,
    ) -> Option<(ActiveFrontend, &'static str)> {
        if cmd == "relaunch" {
            return self.active().cloned().map(|to| (to, "used"));
        }
        let named = SHOWS.contains(&cmd).then(|| args.get("workspace")).flatten().and_then(serde_json::Value::as_str);
        let row = named.and_then(|w| workspaces.resolve(Some(w)));
        self.show_target(row.as_ref().map(|ws| ws.workspace_id.as_str()))
    }

    /// The one window a show from row `row` (a canonical row id; `None` for a command that names no row) goes to, and
    /// why: `row`, the window showing `row` that a person used most recently, if within `ACTIVE_WINDOW`; otherwise
    /// `last-used`, the window a person used most recently, however long ago; otherwise `last-connected`, the window
    /// that connected last. `None` only when no named frontend is attached: a show never goes to every window.
    pub fn show_target(&self, row: Option<&str>) -> Option<(ActiveFrontend, &'static str)> {
        resolve_show(&self.clients, row, self.now)
    }
}

/// `show_target`'s rule, with `now` passed in for tests: one ordering, highest first: (shows `row` and used within
/// `ACTIVE_WINDOW`, last used, connection serial).
fn resolve_show(clients: &[ClientInfo], row: Option<&str>, now: Instant) -> Option<(ActiveFrontend, &'static str)> {
    let on_row = |c: &ClientInfo| {
        row.is_some()
            && c.shown_row.as_deref() == row
            && c.last_person_input_at.is_some_and(|at| now.saturating_duration_since(at) <= ACTIVE_WINDOW)
    };
    let (c, handle) = clients
        .iter()
        .filter(|c| c.role == "fe")
        .filter_map(|c| Some((c, c.name.as_deref().filter(|n| !n.is_empty())?)))
        .max_by_key(|(c, _)| (on_row(c), c.last_person_input_at, c.serial))?;
    let why = match (on_row(c), c.last_person_input_at) {
        (true, _) => "row",
        (false, Some(_)) => "last-used",
        (false, None) => "last-connected",
    };
    Some((ActiveFrontend { serial: c.serial, handle: handle.to_string() }, why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fe(serial: u64, name: &str, row: Option<&str>, used: Option<Instant>) -> ClientInfo {
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
            shown_row: row.map(Into::into),
        }
    }

    fn chosen(clients: &[ClientInfo], row: Option<&str>, now: Instant) -> Option<(String, &'static str)> {
        resolve_show(clients, row, now).map(|(to, why)| (to.handle, why))
    }

    fn pick(handle: &str, why: &'static str) -> Option<(String, &'static str)> {
        Some((handle.to_string(), why))
    }

    #[test]
    fn a_show_goes_to_the_window_on_its_row_over_one_used_later_elsewhere() {
        let now = Instant::now() + Duration::from_secs(3600);
        let s = |secs| Some(now - Duration::from_secs(secs));
        let clients = [fe(1, "fe@a", Some("ws-x"), s(180)), fe(2, "fe@b", Some("ws-y"), s(5))];
        assert_eq!(chosen(&clients, Some("ws-x"), now), pick("fe@a", "row"), "P2: the window on the row");
        assert_eq!(chosen(&clients, None, now), pick("fe@b", "last-used"), "no row: the window used last");
        let both = [fe(1, "fe@a", Some("ws-x"), s(120)), fe(2, "fe@c", Some("ws-x"), s(60)), fe(3, "fe@b", Some("ws-y"), s(1))];
        assert_eq!(chosen(&both, Some("ws-x"), now), pick("fe@c", "row"), "P1: of two on the row, the one used last");
    }

    #[test]
    fn an_idle_window_on_the_row_never_takes_a_show_from_the_window_in_use() {
        let now = Instant::now() + Duration::from_secs(3600);
        let s = |secs| Some(now - Duration::from_secs(secs));
        let clients = [fe(1, "fe@idle", Some("ws-x"), s(301)), fe(2, "fe@b", Some("ws-y"), s(400))];
        assert_eq!(chosen(&clients, Some("ws-x"), now), pick("fe@idle", "last-used"), "no window in use: used last");
        let clients = [fe(1, "fe@idle", Some("ws-x"), s(301)), fe(2, "fe@b", Some("ws-y"), s(10))];
        assert_eq!(chosen(&clients, Some("ws-x"), now), pick("fe@b", "last-used"), "the idle window on the row loses");
        let at_edge = [fe(1, "fe@a", Some("ws-x"), s(300)), fe(2, "fe@b", Some("ws-y"), s(1))];
        assert_eq!(chosen(&at_edge, Some("ws-x"), now), pick("fe@a", "row"), "five minutes still counts");
    }

    #[test]
    fn exactly_one_window_whenever_one_is_named_and_attached() {
        let now = Instant::now() + Duration::from_secs(3600);
        let never = [fe(1, "fe@a", Some("ws-x"), None), fe(2, "fe@b", None, None)];
        assert_eq!(chosen(&never, Some("ws-x"), now), pick("fe@b", "last-connected"), "P3: no window ever used");
        let mut others = [fe(1, "fe@a", None, None), fe(2, "", None, None), fe(3, "fe@c", None, None)];
        others[2].role = "agent".into();
        assert_eq!(chosen(&others, None, now), pick("fe@a", "last-connected"), "only named frontends are chosen");
        others[0].name = None;
        assert_eq!(chosen(&others, None, now), None, "nothing to choose: no window");
    }
}
