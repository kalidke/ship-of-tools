// clients.rs — backend awareness of connected frontends.
//
// One Ship of Tools backend serves many concurrent frontends (ADR 0010/0013:
// "multiple frontends can attach to the same backend simultaneously").
// The wire path already supported this — every accepted stream gets its
// own connection task and all event buses fan out via `broadcast` — but
// nothing on the backend *knew* how many clients were attached. This
// registry closes that gap for the device-roaming case (same user moving
// between desktop and laptop, both pointed at one backend's workspaces).
//
// Each live connection registers itself on `hello` (when its `client_id`
// is first known) and holds a `ClientGuard` for the connection's
// lifetime; the guard deregisters on drop, so every disconnect path —
// clean EOF, transport error, task panic — is covered without an explicit
// teardown call. Registration is keyed by a per-connection serial, not by
// `client_id`, so a machine that reconnects (same `client_id`, new socket)
// is a distinct entry until its old connection task winds down.
//
// The registry is intentionally minimal: a count + a roster for logging
// and the `clients_connected` field in `HelloRes`. Write policy (single-
// writer lock, follower mode) is deliberately *not* built here — for the
// roaming use case both connections are the same user and optimistic
// concurrency on file/concept writes already prevents lost writes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The "active frontend" window (owner-approved design, 2026-09-08): a
/// `last_person_input_at` stamp older than this no longer counts as "a
/// person is here" for `Clients::active_frontend` below. Five minutes —
/// long enough that a person reading a preview without touching a key
/// doesn't get silently demoted, short enough that a box the owner walked
/// away from stops absorbing untargeted commands.
const ACTIVE_WINDOW_SECS: u64 = 5 * 60;

/// One connected frontend, as the backend sees it. `app_version`/`protocol`
/// (ADR 0030 §8 decision 31b) are what `version.query` reports per client —
/// sourced from the hello this connection already sent, never a new probe.
/// `peer` is captured but not yet surfaced on the wire (hence `allow`ed).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClientInfo {
    /// The frontend-supplied, reconnect-stable id (per ADR 0010).
    pub client_id: String,
    /// "local" | "tcp" — the transport this connection arrived on.
    pub transport: &'static str,
    /// Peer address for TCP connections; None for local sockets.
    pub peer: Option<String>,
    /// Unix-epoch seconds the connection registered (hello time).
    pub connected_at: u64,
    /// This client's `HelloReq::app_version`, captured at registration
    /// (first hello on this connection — a later reconnect hello on the
    /// SAME connection keeps the original `register` call, matching
    /// `transport`/`peer`'s own lifetime).
    pub app_version: String,
    /// This client's `HelloReq::protocol`, same capture timing as
    /// `app_version`.
    pub protocol: u32,
    /// This client's `HelloReq::fe_handle` (owner-approved "active
    /// frontend" design, 2026-09-08): the frontend's own `win-fe-<host>`
    /// sot-comm handle, self-reported at hello. `None` for a non-FE
    /// client (a comm script's `sot-comm` hello) or a pre-this-field
    /// frontend — such a client can never become the active frontend
    /// (`active_frontend` requires a handle), only ever an explicit
    /// `--fe <handle>` target reaches it, exactly as before this design.
    pub fe_handle: Option<String>,
    /// Unix-epoch seconds of the most recent request THIS connection sent
    /// that a person, not an agent, generates — see the call sites in
    /// `server.rs` for the exact op list (`pty.write`, `preview.get`,
    /// `tree.root`/`tree.children`, a `workspace.activate` with
    /// `read: true`). `None` until the first such request. This is the
    /// invariant `active_frontend` resolves from: the daemon can name
    /// which frontend a person is using without asking them.
    pub last_person_input_at: Option<u64>,
}

#[derive(Default)]
struct Inner {
    /// Keyed by per-connection serial (NOT client_id) so two live
    /// connections from the same machine are distinct entries.
    by_conn: HashMap<u64, ClientInfo>,
}

/// Shared, cheaply-cloneable handle to the connected-client roster.
#[derive(Clone)]
pub struct Clients {
    inner: Arc<Mutex<Inner>>,
    next_serial: Arc<AtomicU64>,
}

impl Clients {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            next_serial: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Register a connection. Returns a guard that deregisters on drop —
    /// hold it for the connection's lifetime. Logs the new live count and
    /// the distinct `client_id`s currently attached.
    pub fn register(
        &self,
        client_id: impl Into<String>,
        transport: &'static str,
        peer: Option<String>,
        app_version: impl Into<String>,
        protocol: u32,
        fe_handle: Option<String>,
    ) -> ClientGuard {
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let info = ClientInfo {
            client_id: client_id.into(),
            transport,
            peer,
            connected_at: now_secs(),
            app_version: app_version.into(),
            protocol,
            fe_handle,
            last_person_input_at: None,
        };
        let (count, roster) = {
            let mut g = self.inner.lock().unwrap();
            g.by_conn.insert(serial, info.clone());
            (g.by_conn.len(), distinct_client_ids(&g.by_conn))
        };
        tracing::info!(
            client_id = %info.client_id,
            transport = info.transport,
            connections = count,
            distinct_clients = %roster,
            "frontend connected"
        );
        ClientGuard {
            inner: self.inner.clone(),
            serial,
            client_id: info.client_id,
        }
    }

    /// Number of live connections (not distinct clients — a reconnecting
    /// machine can briefly count twice until its old task winds down).
    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().by_conn.len()
    }

    /// Snapshot of every connected client — `version.query`'s `clients[]`
    /// roster (ADR 0030 §8 decision 31b).
    pub fn snapshot(&self) -> Vec<ClientInfo> {
        self.inner.lock().unwrap().by_conn.values().cloned().collect()
    }

    /// Stamp `last_person_input_at = now` for the connection at `serial`
    /// (owner-approved "active frontend" design, 2026-09-08). Call this
    /// ONLY from a request a person, not an agent, generates — see the
    /// call sites in `server.rs`. A no-op if `serial` isn't registered
    /// (already disconnected, or `hello` hasn't landed yet).
    pub fn touch_person_input(&self, serial: u64) {
        let mut g = self.inner.lock().unwrap();
        if let Some(info) = g.by_conn.get_mut(&serial) {
            info.last_person_input_at = Some(now_secs());
        }
    }

    /// The active frontend (owner-approved design, 2026-09-08): the
    /// `fe_handle` of whichever client has one AND the most recent
    /// `last_person_input_at` within `ACTIVE_WINDOW_SECS` of now. `None`
    /// when no client qualifies — no frontend has reported a handle yet,
    /// or none has had person input inside the window. A client with no
    /// `fe_handle` is never a candidate, so a pre-this-field frontend (or
    /// a non-FE client) never becomes "the active frontend" even if it's
    /// the only thing touching the daemon.
    pub fn active_frontend(&self) -> Option<String> {
        let now = now_secs();
        self.inner
            .lock()
            .unwrap()
            .by_conn
            .values()
            .filter_map(|c| {
                let handle = c.fe_handle.clone()?;
                let at = c.last_person_input_at?;
                (now.saturating_sub(at) <= ACTIVE_WINDOW_SECS).then_some((at, handle))
            })
            .max_by_key(|(at, _)| *at)
            .map(|(_, handle)| handle)
    }
}

impl Default for Clients {
    fn default() -> Self {
        Self::new()
    }
}

/// Deregisters its connection when dropped. One per connection task.
pub struct ClientGuard {
    inner: Arc<Mutex<Inner>>,
    serial: u64,
    client_id: String,
}

impl ClientGuard {
    /// This connection's per-connection serial — the key the `docs.open` site
    /// map uses (ADR 0029). Threaded into `handle_docs_open` so the returned URL
    /// carries it, and used by `Drop` below to reap the entry on disconnect.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// This connection's own `hello` `client_id` — ADR 0042 amendment
    /// (2026-09-07): `pty.input`'s default `controller_id` when the request
    /// carries no explicit `origin`. Attribution, not authentication — the
    /// same trust level `client_id` has always carried.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let (count, roster) = {
            let mut g = self.inner.lock().unwrap();
            g.by_conn.remove(&self.serial);
            (g.by_conn.len(), distinct_client_ids(&g.by_conn))
        };
        // Reap this connection's docs.open site root (ADR 0029). The map is keyed
        // by serial, so this drops exactly the departing connection's entry — the
        // only cleanup site needed (the ADR-0027 reaper guarantees we reach Drop
        // even for half-open / hung peers, so this inherits its coverage).
        crate::site_serve::remove_root(self.serial);
        tracing::info!(
            client_id = %self.client_id,
            connections = count,
            distinct_clients = %roster,
            "frontend disconnected"
        );
    }
}

fn distinct_client_ids(by_conn: &HashMap<u64, ClientInfo>) -> String {
    let mut ids: Vec<&str> = by_conn.values().map(|c| c.client_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.join(",")
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_drop_track_count() {
        let clients = Clients::new();
        assert_eq!(clients.count(), 0);

        let g1 = clients.register("client-a", "tcp", Some("127.0.0.1:5000".into()), "0.6.0", 1, None);
        assert_eq!(clients.count(), 1);

        let g2 = clients.register("client-b", "local", None, "0.6.0", 1, None);
        assert_eq!(clients.count(), 2);
        assert_eq!(clients.snapshot().len(), 2);

        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn same_client_id_two_connections_are_distinct() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "tcp", None, "0.6.0", 1, None);
        let g2 = clients.register("client-a", "tcp", None, "0.6.0", 1, None);
        // Two live connections, one distinct client.
        assert_eq!(clients.count(), 2);
        assert_eq!(distinct_client_ids(&clients.inner.lock().unwrap().by_conn), "client-a");
        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn snapshot_carries_app_version_and_protocol() {
        // Decision 31b: this is exactly what `version.query`'s `clients[]`
        // roster reads — sourced from registration, not a new probe.
        let clients = Clients::new();
        let _g = clients.register("client-a", "tcp", None, "0.6.0-dev+abc1234", 1, None);
        let snap = clients.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].app_version, "0.6.0-dev+abc1234");
        assert_eq!(snap[0].protocol, 1);
    }

    #[test]
    fn touch_person_input_stamps_only_the_named_serial() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let _g2 = clients.register("client-b", "local", None, "0.6.0", 1, Some("win-fe-b".into()));

        assert!(clients.snapshot().iter().all(|c| c.last_person_input_at.is_none()));

        clients.touch_person_input(g1.serial());
        let snap = clients.snapshot();
        let a = snap.iter().find(|c| c.client_id == "client-a").unwrap();
        let b = snap.iter().find(|c| c.client_id == "client-b").unwrap();
        assert!(a.last_person_input_at.is_some(), "the touched connection is stamped");
        assert!(b.last_person_input_at.is_none(), "an untouched connection stays unstamped");
    }

    #[test]
    fn touch_person_input_on_a_departed_serial_is_a_harmless_noop() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let serial = g.serial();
        drop(g);
        // Must not panic on a serial that no longer has an entry.
        clients.touch_person_input(serial);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn active_frontend_requires_both_a_handle_and_a_fresh_stamp() {
        let clients = Clients::new();
        // No handle at all: never active, even though it's touched.
        let no_handle = clients.register("client-a", "local", None, "0.6.0", 1, None);
        clients.touch_person_input(no_handle.serial());
        assert_eq!(clients.active_frontend(), None, "a client with no fe_handle is never active");

        // A handle but never touched: not active either.
        let untouched = clients.register("client-b", "local", None, "0.6.0", 1, Some("win-fe-b".into()));
        let _ = &untouched;
        assert_eq!(clients.active_frontend(), None);
    }

    #[test]
    fn active_frontend_ties_resolve_to_the_most_recent_stamp() {
        let clients = Clients::new();
        let older = clients.register("client-a", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        let newer = clients.register("client-b", "local", None, "0.6.0", 1, Some("win-fe-b".into()));

        // Stamp the "older" one first, then the "newer" one a moment later
        // (both real wall-clock seconds, so this only asserts ordering, not
        // an exact gap) — the more recently touched frontend wins.
        clients.touch_person_input(older.serial());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        clients.touch_person_input(newer.serial());

        assert_eq!(clients.active_frontend(), Some("win-fe-b".to_string()));
    }

    #[test]
    fn active_frontend_ignores_a_stamp_outside_the_window() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, Some("win-fe-a".into()));
        {
            // Reach in and backdate the stamp past ACTIVE_WINDOW_SECS —
            // sleeping the real window in a unit test isn't practical.
            let mut inner = clients.inner.lock().unwrap();
            inner.by_conn.get_mut(&g.serial()).unwrap().last_person_input_at =
                Some(now_secs().saturating_sub(ACTIVE_WINDOW_SECS + 1));
        }
        assert_eq!(
            clients.active_frontend(),
            None,
            "a stamp older than the active window no longer counts as \"a person is here\""
        );
    }
}
