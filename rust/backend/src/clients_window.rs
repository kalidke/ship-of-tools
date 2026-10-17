//! A window's state on this daemon, which outlives each of its connections: a window that redials (its link dropped,
//! its computer woke) is the same window, so the REPL runs it numbered, when a person last used it and the pages it
//! opened carry over to its new connection. A window is its hello's declared host and `instance` (ADR
//! 0046 decision 1: the frontend makes the instance once and sends it on every connection). A connection whose hello
//! names no instance is a window of its own, whose state ends with it.

use super::{ClientInfo, Inner};
use crate::sidecars::repl::numbers::CallerNumbers;
use std::time::Instant;

/// How many windows with no live connection the roster remembers; past this many, the one away longest is forgotten,
/// its pages end, and if it comes back it starts afresh. Far more than the windows one person has away at once
/// (asleep, redialing), and each is small (`CallerNumbers` keeps at most 256 runs; a window owns at most one page per
/// page server, ADR 0029).
const AWAY: usize = 64;

/// A window: its hello's declared host and instance.
pub(super) type WindowKey = (String, String);

/// What a window carries from one connection to the next.
#[derive(Default)]
pub(super) struct Window {
    /// The REPL runs the window numbered itself, shared by each of its connections.
    numbers: CallerNumbers,
    /// The owner of the pages the window opened (`docs.open`'s sites, `pages::site`): the serial of the connection
    /// that made this record, which names no other connection, so each connection of the window opens and keeps the
    /// same pages, and they end when the record is forgotten.
    sites: u64,
    /// When a person last used the window (`fe.presence`): a new connection of the window starts with it, and the
    /// connection that reports a new one sets both copies.
    last_person_input_at: Option<Instant>,
    /// How many of its connections are live, and when the last of them left.
    live: usize,
    left_at: Option<Instant>,
}

/// The window `info`'s connection belongs to, if its hello named one.
fn window_key(info: &ClientInfo) -> Option<WindowKey> {
    Some((info.host.clone()?, info.instance.clone().filter(|i| !i.is_empty())?))
}

/// How many windows `infos` are: a window's connections count once, a connection of no window counts as its own.
pub(super) fn window_count<'a>(infos: impl Iterator<Item = &'a ClientInfo>) -> usize {
    infos.map(|info| window_key(info).ok_or(info.serial)).collect::<std::collections::HashSet<_>>().len()
}

impl super::Clients {
    /// The owners of the pages of the windows with no live connection, the one away longest first: whose pool port a
    /// new open may take when every port is held (`pages::site::assign_pool_port`). Never a live window's.
    pub fn away_sites(&self) -> Vec<u64> {
        let inner = self.inner.lock().unwrap();
        let away = inner.windows.values().filter(|w| w.live == 0);
        let mut away: Vec<_> = away.filter_map(|w| Some((w.left_at?, w.sites))).collect();
        away.sort_unstable();
        away.into_iter().map(|(_, sites)| sites).collect()
    }
}

impl Inner {
    /// A connection of `info`'s window arrives: `info` takes the window's presence, and the connection gets
    /// the window's numbers and the owner of its pages. A connection of no window gets numbers of its own and owns its
    /// pages by its own serial.
    pub(super) fn arrive(&mut self, info: &mut ClientInfo) -> (CallerNumbers, u64) {
        let Some(key) = window_key(info) else {
            return (CallerNumbers::default(), info.serial);
        };
        let sites = info.serial;
        let window = self.windows.entry(key).or_insert_with(|| Window { sites, ..Window::default() });
        window.live += 1;
        window.left_at = None;
        info.last_person_input_at = window.last_person_input_at;
        (window.numbers.clone(), window.sites)
    }

    /// A connection of `info`'s window has left. When it was the last, the window is remembered as away, and past
    /// `AWAY` windows away the one away longest is forgotten. Returns the owner whose pages end with this departure: a
    /// connection of no window's own, or a forgotten window's.
    pub(super) fn leave(&mut self, info: &ClientInfo) -> Option<u64> {
        let Some(key) = window_key(info) else {
            return Some(info.serial);
        };
        let window = self.windows.get_mut(&key)?;
        window.live = window.live.saturating_sub(1);
        if window.live > 0 {
            return None;
        }
        window.left_at = Some(Instant::now());
        if self.windows.values().filter(|w| w.live == 0).count() <= AWAY {
            return None;
        }
        let longest = self.windows.iter().filter_map(|(key, w)| Some((w.left_at?, key))).min();
        let key = longest.map(|(_, key)| key.clone())?;
        self.windows.remove(&key).map(|w| w.sites)
    }

    /// `serial`'s connection reports a person at its window at `at`: it lands on the connection and on its window. A
    /// no-op for an unregistered serial.
    pub(super) fn report(&mut self, serial: u64, at: Instant) {
        let Some(info) = self.by_conn.get_mut(&serial) else {
            return;
        };
        info.last_person_input_at = Some(at);
        if let Some(window) = window_key(info).and_then(|key| self.windows.get_mut(&key)) {
            window.last_person_input_at = Some(at);
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

    /// A window's connections own its pages by one owner, the second arriving after the first has left (a redial)
    /// included, and a window away is listed as away until it comes back; another window (the same instance on another
    /// host included), or a connection of no window, owns its pages by its own serial.
    #[test]
    fn a_windows_connections_share_the_owner_of_its_pages() {
        let clients = Clients::new();
        let first = connect(&clients, Some("w"));
        let owner = first.site_owner();
        drop(first);
        assert_eq!(clients.away_sites(), vec![owner], "the window is away");
        let again = connect(&clients, Some("w"));
        assert_eq!(again.site_owner(), owner, "the redial owns the window's pages");
        assert!(clients.away_sites().is_empty(), "a window with a live connection is not away");
        let name = Some("fe@host-b".to_string());
        let elsewhere = clients.register("c", "0.6.6", 1, "fe".into(), Some("host-b".into()), Some("w".into()), name);
        for other in [connect(&clients, Some("v")), connect(&clients, None), connect(&clients, Some("")), elsewhere] {
            assert_eq!(other.site_owner(), other.serial(), "not the window's");
        }
    }

    /// A redialed window starts as it was: used when a person last used it.
    #[test]
    fn a_redialed_window_keeps_its_presence() {
        let clients = Clients::new();
        let first = connect(&clients, Some("w"));
        clients.touch_person_input(first.serial());
        let used = clients.snapshot_with_active().clients[0].last_person_input_at;
        assert!(used.is_some());
        drop(first);
        let again = connect(&clients, Some("w"));
        let snap = clients.snapshot_with_active();
        assert_eq!(snap.clients[0].last_person_input_at, used);
        assert_eq!(snap.active().map(|a| a.serial), Some(again.serial()), "still the window in use");
        let alone = connect(&clients, None);
        let snap = clients.snapshot_with_active();
        let row = snap.clients.iter().find(|c| c.serial == alone.serial()).expect("its row");
        assert_eq!(row.last_person_input_at, None, "a connection of no window");
    }

    /// Of a window's two live connections (an old one not yet reaped and its redial), the newer gets what goes to the
    /// window.
    #[test]
    fn of_a_windows_connections_the_newest_is_chosen() {
        let clients = Clients::new();
        let old = connect(&clients, Some("w"));
        clients.touch_person_input(old.serial());
        let new = connect(&clients, Some("w"));
        let snap = clients.snapshot_with_active();
        assert_eq!(snap.active().map(|a| a.serial), Some(new.serial()));
        assert_eq!(snap.in_use().map(|(to, why)| (to.serial, why)), Some((new.serial(), "last-used")));
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
        let first_owner = first_away.site_owner();
        drop(first_away);
        let mut owners = Vec::new();
        for i in 1..=AWAY {
            let away = connect(&clients, Some(&format!("away-{i}")));
            owners.push(away.site_owner());
            drop(away);
        }
        let windows = clients.inner.lock().unwrap().windows.len();
        assert_eq!(windows, AWAY + 1, "the live window and AWAY windows away");
        let away = clients.away_sites();
        assert_eq!(away, owners, "the windows away, the one away longest first");
        assert!(!away.contains(&first_owner), "the forgotten window is not away");
        assert!(!away.contains(&live.site_owner()), "the live window is not away");
        assert_eq!(connect(&clients, Some("away-0")).numbers().daemon_numbers(&[1]), vec![1], "the one away longest went");
        assert_eq!(connect(&clients, Some("live")).numbers().daemon_numbers(&[1]), vec![n], "the live window stays");
    }
}
