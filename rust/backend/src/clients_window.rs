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
#[path = "clients_window_tests.rs"]
mod tests;
