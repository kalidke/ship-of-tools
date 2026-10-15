//! A window's state on this daemon, which outlives each of its connections: a window that redials (its link dropped,
//! its computer woke) is the same window, so the REPL runs it numbered, when a person last used it and the row it shows
//! carry over to its new connection. A window is its hello's declared host and `instance` (ADR 0046 decision 1: the
//! frontend makes the instance once and sends it on every connection). A connection whose hello names no instance is a
//! window of its own, whose state ends with it.

use super::{ClientInfo, Inner};
use crate::sidecars::repl::numbers::CallerNumbers;
use std::time::Instant;

/// How many windows with no live connection the roster remembers; past this many, the one away longest is forgotten,
/// and if it comes back it starts afresh. Far more than the windows one person has away at once (asleep, redialing),
/// and each is small (`CallerNumbers` keeps at most 256 runs).
const AWAY: usize = 64;

/// A window: its hello's declared host and instance.
pub(super) type WindowKey = (String, String);

/// What a window carries from one connection to the next.
#[derive(Default)]
pub(super) struct Window {
    /// The REPL runs the window numbered itself, shared by each of its connections.
    numbers: CallerNumbers,
    /// When a person last used the window (`fe.presence`) and the row it shows (`workspace.activate`): a new
    /// connection of the window starts with them, and the connection that reports a new one sets both copies.
    last_person_input_at: Option<Instant>,
    shown_row: Option<String>,
    /// How many of its connections are live, and when the last of them left.
    live: usize,
    left_at: Option<Instant>,
}

/// The window `info`'s connection belongs to, if its hello named one.
fn window_key(info: &ClientInfo) -> Option<WindowKey> {
    Some((info.host.clone()?, info.instance.clone().filter(|i| !i.is_empty())?))
}

impl Inner {
    /// A connection of `info`'s window arrives: `info` takes the window's presence and row, and the connection gets
    /// the window's numbers. A connection of no window gets numbers of its own.
    pub(super) fn arrive(&mut self, info: &mut ClientInfo) -> CallerNumbers {
        let Some(key) = window_key(info) else {
            return CallerNumbers::default();
        };
        let window = self.windows.entry(key).or_default();
        window.live += 1;
        window.left_at = None;
        info.last_person_input_at = window.last_person_input_at;
        info.shown_row.clone_from(&window.shown_row);
        window.numbers.clone()
    }

    /// A connection of `info`'s window has left. When it was the last, the window is remembered as away, and past
    /// `AWAY` windows away the one away longest is forgotten.
    pub(super) fn leave(&mut self, info: &ClientInfo) {
        let Some(window) = window_key(info).and_then(|key| self.windows.get_mut(&key)) else {
            return;
        };
        window.live = window.live.saturating_sub(1);
        if window.live > 0 {
            return;
        }
        window.left_at = Some(Instant::now());
        if self.windows.values().filter(|w| w.live == 0).count() > AWAY {
            let longest = self.windows.iter().filter_map(|(key, w)| Some((w.left_at?, key))).min();
            if let Some(key) = longest.map(|(_, key)| key.clone()) {
                self.windows.remove(&key);
            }
        }
    }

    /// `serial`'s connection reports its window's presence or row (`set`): it lands on the connection and on its
    /// window. A no-op for an unregistered serial.
    pub(super) fn report(&mut self, serial: u64, set: impl Fn(&mut Option<Instant>, &mut Option<String>)) {
        let Some(info) = self.by_conn.get_mut(&serial) else {
            return;
        };
        set(&mut info.last_person_input_at, &mut info.shown_row);
        if let Some(window) = window_key(info).and_then(|key| self.windows.get_mut(&key)) {
            set(&mut window.last_person_input_at, &mut window.shown_row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Clients;
    use super::*;

    fn connect(clients: &Clients, instance: Option<&str>) -> super::super::ClientGuard {
        let name = Some("fe@host-a".to_string());
        clients.register("c", "0.6.6", 1, "fe".into(), Some("host-a".into()), instance.map(Into::into), name)
    }

    /// Two connections of one window share its numbers, the second arriving after the first has left (a redial)
    /// included; another window, or a connection of no window, has numbers of its own.
    #[test]
    fn a_windows_connections_share_its_numbers_and_no_one_else_does() {
        let clients = Clients::new();
        let first = connect(&clients, Some("w"));
        let n = first.numbers().number(Some(1));
        drop(first);
        let again = connect(&clients, Some("w"));
        assert_eq!(again.numbers().daemon_numbers(&[1]), vec![n], "the redial's numbers are the window's");
        for other in [connect(&clients, Some("v")), connect(&clients, None), connect(&clients, Some(""))] {
            assert_eq!(other.numbers().daemon_numbers(&[1]), vec![1], "not the window's");
        }
    }

    /// A redialed window starts as it was: used when a person last used it, showing the row it showed.
    #[test]
    fn a_redialed_window_keeps_its_presence_and_row() {
        let clients = Clients::new();
        let first = connect(&clients, Some("w"));
        clients.touch_person_input(first.serial());
        clients.show_row(first.serial(), "ws-x".into());
        let used = clients.snapshot_with_active().clients[0].last_person_input_at;
        assert!(used.is_some());
        drop(first);
        let again = connect(&clients, Some("w"));
        let snap = clients.snapshot_with_active();
        assert_eq!((snap.clients[0].last_person_input_at, snap.clients[0].shown_row.as_deref()), (used, Some("ws-x")));
        assert_eq!(snap.active().map(|a| a.serial), Some(again.serial()), "still the window in use");
        let alone = connect(&clients, None);
        let snap = clients.snapshot_with_active();
        let row = snap.clients.iter().find(|c| c.serial == alone.serial()).expect("its row");
        assert_eq!((row.last_person_input_at, row.shown_row.as_deref()), (None, None), "a connection of no window");
    }

    /// Of a window's two live connections (an old one not yet reaped and its redial), the newer gets what goes to the
    /// window.
    #[test]
    fn of_a_windows_connections_the_newest_is_chosen() {
        let clients = Clients::new();
        let old = connect(&clients, Some("w"));
        clients.touch_person_input(old.serial());
        clients.show_row(old.serial(), "ws-x".into());
        let new = connect(&clients, Some("w"));
        let snap = clients.snapshot_with_active();
        assert_eq!(snap.active().map(|a| a.serial), Some(new.serial()));
        assert_eq!(snap.show_target(Some("ws-x")).map(|(to, why)| (to.serial, why)), Some((new.serial(), "row")));
    }

    /// The roster remembers at most `AWAY` windows with no live connection, forgetting the one away longest; a window
    /// with a live connection is never forgotten.
    #[test]
    fn windows_away_are_bounded_and_a_live_window_is_kept() {
        let clients = Clients::new();
        let live = connect(&clients, Some("live"));
        let n = live.numbers().number(Some(1));
        let first_away = connect(&clients, Some("away-0"));
        first_away.numbers().number(Some(1));
        drop(first_away);
        for i in 1..=AWAY {
            drop(connect(&clients, Some(&format!("away-{i}"))));
        }
        let windows = clients.inner.lock().unwrap().windows.len();
        assert_eq!(windows, AWAY + 1, "the live window and AWAY windows away");
        assert_eq!(connect(&clients, Some("away-0")).numbers().daemon_numbers(&[1]), vec![1], "the one away longest went");
        assert_eq!(connect(&clients, Some("live")).numbers().daemon_numbers(&[1]), vec![n], "the live window stays");
    }
}
