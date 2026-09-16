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
//
// "Active frontend" (2026-09-08 review rework of the same-day design):
// which connection is `fe.presence`'s most recent recipient, resolved by
// SERIAL (never a bare handle string — two connections can share one,
// e.g. a stale reconnect) with monotonic sub-second stamps so concurrent
// touches order correctly. See `touch_person_input` and
// `snapshot_with_active`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The "active frontend" window: a `last_person_input_at` stamp older than
/// this no longer counts as "a person is here" for `snapshot_with_active`
/// below. Five minutes — long enough that a person reading a preview
/// without touching a key doesn't get silently demoted, short enough that
/// a box the owner walked away from stops absorbing untargeted commands.
const ACTIVE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// One connected frontend, as the backend sees it. `app_version`/`protocol`
/// (ADR 0030 §8 decision 31b) are what `version.query` reports per client —
/// sourced from the hello this connection already sent, never a new probe.
/// `peer` is captured but not yet surfaced on the wire (hence `allow`ed).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClientInfo {
    /// This connection's own registration serial (mirrors the `by_conn`
    /// map key onto the value itself) — carried here so a caller holding
    /// only a `ClientInfo` from a `ClientsSnapshot` can still compare it
    /// against `ClientsSnapshot::active()`'s serial without a second
    /// registry lookup.
    pub serial: u64,
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
    /// This connection's declared `HelloReq::host` (ADR 0046 decision 1) —
    /// read from the peer's own declaration, never recomputed. `None` for
    /// a pre-this-field peer. Display/log surface only.
    pub host: Option<String>,
    /// This connection's declared `HelloReq::role`: `"fe"` a frontend,
    /// `"bridge"` a session's listener loop, `"cli"` a one-shot shell
    /// call, `"agent"` a one-shot in-session call. Only `"fe"` rows are
    /// ever selected as the active frontend or counted as a directed
    /// command's audience.
    pub role: String,
    /// This connection's declared `HelloReq::instance` (ADR 0046 decision
    /// 1) — opaque per-process discriminator, `None` for a role with no
    /// notion of instance or a pre-this-field peer. Display/log only.
    pub instance: Option<String>,
    /// This connection's declared `HelloReq::name`: `fe@<host>` for a
    /// frontend (the address `--fe <host>` scopes a command to), a
    /// bridge/cli/agent's own sot-comm handle otherwise. `None` for a
    /// role that declared nothing — such a frontend can never be
    /// SELECTED as the active frontend (`snapshot_with_active`), though a
    /// genuine broadcast still reaches it like every other connection.
    pub name: Option<String>,
    /// Monotonic instant of the most recent `fe.presence` this connection
    /// sent (2026-09-08 review rework) — `None` until the first one.
    /// `Instant`, not wall-clock time: sub-second precision so two
    /// touches close together still order correctly, and immune to clock
    /// adjustment. This is the ONLY thing that sets this field — no other
    /// op stamps it (an earlier design inferred presence from ordinary
    /// navigation/typing ops; every one of them turned out to have an
    /// automated producer too, so it was deleted).
    pub last_person_input_at: Option<Instant>,
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
        role: String,
        host: Option<String>,
        instance: Option<String>,
        name: Option<String>,
    ) -> ClientGuard {
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let info = ClientInfo {
            serial,
            client_id: client_id.into(),
            transport,
            peer,
            connected_at: now_secs(),
            app_version: app_version.into(),
            protocol,
            role,
            host,
            instance,
            name,
            last_person_input_at: None,
        };
        let (count, roster) = {
            let mut g = self.inner.lock().unwrap();
            g.by_conn.insert(serial, info.clone());
            (g.by_conn.len(), distinct_client_ids(&g.by_conn))
        };
        tracing::info!(
            client_id = %info.client_id,
            name = ?info.name,
            role = %info.role,
            host = ?info.host,
            instance = ?info.instance,
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

    /// Connection names positioned to receive an `agent.send` addressed to
    /// `to` (`to == ""` is the broadcast form — see `AgentSendReq`), read
    /// with ONE lock acquisition. A row counts when its `serial` isn't
    /// `self_serial` (a sender is never its own receiver) AND either its
    /// declared `name` matches `to` exactly, or its `role` is `"fe"`
    /// unconditionally — a Windows-hosted handle runs no bridge (its
    /// frontend files every broadcast frame straight into its own inbox
    /// rather than filtering client-side like a bridge does), so gating
    /// `"fe"` rows on a name match would turn every send to a Windows
    /// session into a false "no receiver". `to == ""` counts every OTHER
    /// row with a non-empty declared `name`. Rows with no declared `name`
    /// never appear in the result — there's nothing to report them as.
    pub fn receivers_for(&self, to: &str, self_serial: u64) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        g.by_conn
            .values()
            .filter(|c| c.serial != self_serial)
            .filter(|c| to.is_empty() || c.name.as_deref() == Some(to) || c.role == "fe")
            .filter_map(|c| c.name.clone())
            .collect()
    }

    /// Stamp `last_person_input_at = now` for the connection at `serial` —
    /// the ONLY thing that should call this is the `fe.presence` handler
    /// (2026-09-08 review rework, design point A). A no-op if `serial`
    /// isn't registered (already disconnected, or `hello` hasn't landed
    /// yet).
    pub fn touch_person_input(&self, serial: u64) {
        let mut g = self.inner.lock().unwrap();
        if let Some(info) = g.by_conn.get_mut(&serial) {
            info.last_person_input_at = Some(Instant::now());
        }
    }

    /// One consistent read of the registry: every connection (the
    /// `version.query` roster) PLUS the active frontend, resolved from the
    /// SAME lock acquisition and the SAME `Instant::now()` — so the roster
    /// and "who's active" can never disagree about what "now" meant
    /// (2026-09-08 review, finding 6). Call this once per request that
    /// needs either piece, never `clients()`-then-`active()` as two
    /// separate reads.
    pub fn snapshot_with_active(&self) -> ClientsSnapshot {
        let now = Instant::now();
        let clients: Vec<ClientInfo> = self.inner.lock().unwrap().by_conn.values().cloned().collect();
        let active = Self::resolve_active(&clients, now);
        ClientsSnapshot { clients, active }
    }

    /// Pure resolution: the `"fe"`-role, named connection with the most
    /// recent `last_person_input_at` within `ACTIVE_WINDOW` of `now`. A
    /// bridge/cli/agent's `name` never qualifies. Ties resolve to the
    /// LATER-REGISTERED connection — `serial` is monotonically increasing,
    /// so ordering by `(instant, serial)` picks it automatically on an
    /// exact `Instant` tie — without a separate tie-break rule. Factored
    /// out of `snapshot_with_active` (which always passes real
    /// `Instant::now()`) so tests can inject an exact `now` instead of
    /// racing the wall clock for boundary/ordering cases.
    fn resolve_active(clients: &[ClientInfo], now: Instant) -> Option<ActiveFrontend> {
        clients
            .iter()
            .filter_map(|c| {
                if c.role != "fe" {
                    return None;
                }
                let handle = c.name.as_ref()?;
                let at = c.last_person_input_at?;
                (now.saturating_duration_since(at) <= ACTIVE_WINDOW).then_some((at, c.serial, handle))
            })
            .max_by_key(|(at, serial, _)| (*at, *serial))
            .map(|(_, serial, handle)| ActiveFrontend {
                serial,
                handle: handle.clone(),
            })
    }
}

impl Default for Clients {
    fn default() -> Self {
        Self::new()
    }
}

/// The connection an untargeted `fe.command.send` resolves to: identified
/// by SERIAL (the only reliable identity — see the module doc), with its
/// declared `name` carried along for the wire's `target` field and for
/// `sot-fe version`'s display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveFrontend {
    pub serial: u64,
    pub handle: String,
}

/// One consistent snapshot of the client registry — see
/// `Clients::snapshot_with_active`.
pub struct ClientsSnapshot {
    pub clients: Vec<ClientInfo>,
    active: Option<ActiveFrontend>,
}

impl ClientsSnapshot {
    /// The active frontend this snapshot resolved, if any.
    pub fn active(&self) -> Option<&ActiveFrontend> {
        self.active.as_ref()
    }

    /// Is `serial` the active frontend in this snapshot? Roster rows use
    /// this (never a handle comparison) so a duplicate-handle connection
    /// that ISN'T the winner is never marked active alongside it.
    pub fn is_active_serial(&self, serial: u64) -> bool {
        self.active.as_ref().is_some_and(|a| a.serial == serial)
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

/// Wall-clock seconds for `connected_at` (informational/logging only — the
/// active-frontend resolution above uses `Instant`, never this).
fn now_secs() -> u64 {
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

        let g1 = clients.register("client-a", "tcp", Some("127.0.0.1:5000".into()), "0.6.0", 1, String::new(), None, None, None);
        assert_eq!(clients.count(), 1);

        let g2 = clients.register("client-b", "local", None, "0.6.0", 1, String::new(), None, None, None);
        assert_eq!(clients.count(), 2);
        assert_eq!(clients.snapshot_with_active().clients.len(), 2);

        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn same_client_id_two_connections_are_distinct() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "tcp", None, "0.6.0", 1, String::new(), None, None, None);
        let g2 = clients.register("client-a", "tcp", None, "0.6.0", 1, String::new(), None, None, None);
        // Two live connections, one distinct client.
        assert_eq!(clients.count(), 2);
        assert_eq!(distinct_client_ids(&clients.inner.lock().unwrap().by_conn), "client-a");
        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn snapshot_carries_app_version_protocol_and_own_serial() {
        // Decision 31b: this is exactly what `version.query`'s `clients[]`
        // roster reads — sourced from registration, not a new probe.
        let clients = Clients::new();
        let g = clients.register("client-a", "tcp", None, "0.6.0-dev+abc1234", 1, String::new(), None, None, None);
        let snap = clients.snapshot_with_active();
        assert_eq!(snap.clients.len(), 1);
        assert_eq!(snap.clients[0].app_version, "0.6.0-dev+abc1234");
        assert_eq!(snap.clients[0].protocol, 1);
        assert_eq!(snap.clients[0].serial, g.serial());
    }

    #[test]
    fn touch_person_input_stamps_only_the_named_serial() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        let _g2 = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-b".into()));

        assert!(clients
            .snapshot_with_active()
            .clients
            .iter()
            .all(|c| c.last_person_input_at.is_none()));

        clients.touch_person_input(g1.serial());
        let snap = clients.snapshot_with_active();
        let a = snap.clients.iter().find(|c| c.client_id == "client-a").unwrap();
        let b = snap.clients.iter().find(|c| c.client_id == "client-b").unwrap();
        assert!(a.last_person_input_at.is_some(), "the touched connection is stamped");
        assert!(b.last_person_input_at.is_none(), "an untouched connection stays unstamped");
    }

    #[test]
    fn touch_person_input_on_a_departed_serial_is_a_harmless_noop() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
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
        let no_handle = clients.register("client-a", "local", None, "0.6.0", 1, String::new(), None, None, None);
        clients.touch_person_input(no_handle.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a client with no name is never active"
        );

        // A handle but never touched: not active either.
        let untouched = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-b".into()));
        let _ = &untouched;
        assert_eq!(clients.snapshot_with_active().active(), None);
    }

    #[test]
    fn active_excludes_a_named_connection_whose_role_is_not_fe() {
        // `name` alone never makes a connection a frontend: a cli/agent/
        // bridge's declared handle is not an address a command lands on.
        let clients = Clients::new();
        let g = clients.register(
            "client-a",
            "local",
            None,
            "0.6.0",
            1,
            "cli".to_string(),
            None,
            None,
            Some("not-really-a-frontend".into()),
        );
        clients.touch_person_input(g.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a non-fe role is never active even with a name and a fresh touch"
        );
    }

    #[test]
    fn active_requires_the_fe_role_even_when_named_and_touched() {
        // An agent's declared `name` must never make its connection
        // selectable as the active frontend, touched or not.
        let clients = Clients::new();
        let g = clients.register(
            "client-a",
            "local",
            None,
            "0.6.0",
            1,
            "agent".to_string(),
            None,
            None,
            Some("test-host-agent".into()),
        );
        clients.touch_person_input(g.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a declared `name` on a non-fe role is never active"
        );
    }

    /// A directly-constructed `ClientInfo` for `Clients::resolve_active`'s
    /// pure-function tests below — bypasses the registry lock and real
    /// `Instant::now()` entirely, so `now` and every stamp are exactly the
    /// values the test chose (2026-09-08 review, finding 8: "inject the
    /// clock; no sleeps").
    fn ci(serial: u64, handle: &str, last_person_input_at: Option<Instant>) -> ClientInfo {
        ClientInfo {
            serial,
            client_id: format!("client-{serial}"),
            transport: "local",
            peer: None,
            connected_at: 0,
            app_version: "0.6.0".into(),
            protocol: 1,
            role: "fe".to_string(),
            host: None,
            instance: None,
            name: Some(handle.to_string()),
            last_person_input_at,
        }
    }

    #[test]
    fn active_frontend_prefers_the_more_recent_stamp_no_sleeps() {
        // Deterministic ordering, one fixed `now`, no real waiting
        // (2026-09-08 review, finding 8: the prior version of this test
        // slept 1.1s to dodge a same-second tie).
        let now = Instant::now();
        let clients = vec![
            ci(1, "fe@host-a", Some(now - Duration::from_secs(10))),
            ci(2, "fe@host-b", Some(now - Duration::from_secs(1))),
        ];
        let active = Clients::resolve_active(&clients, now);
        assert_eq!(
            active,
            Some(ActiveFrontend { serial: 2, handle: "fe@host-b".to_string() }),
            "the more recently touched frontend wins, identified by its serial"
        );
    }

    #[test]
    fn active_frontend_equal_stamp_ties_go_to_the_later_registered_connection() {
        // "on equal stamps, the later-registered connection wins" —
        // registration order is `serial`, which `resolve_active`'s
        // `(instant, serial)` ordering uses as its tie-break.
        let tie = Instant::now();
        let clients = vec![ci(1, "fe@host-a", Some(tie)), ci(2, "fe@host-b", Some(tie))];
        let active = Clients::resolve_active(&clients, tie);
        assert_eq!(
            active,
            Some(ActiveFrontend { serial: 2, handle: "fe@host-b".to_string() }),
            "an exact tie goes to the later-registered (higher-serial) connection"
        );
    }

    #[test]
    fn active_frontend_window_boundary_exactly_in_then_one_past() {
        let now = Instant::now();
        let exactly_at_window = vec![ci(1, "fe@host-a", Some(now - ACTIVE_WINDOW))];
        assert!(
            Clients::resolve_active(&exactly_at_window, now).is_some(),
            "a stamp exactly ACTIVE_WINDOW old is still active (inclusive boundary)"
        );

        let one_past_window = vec![ci(1, "fe@host-a", Some(now - ACTIVE_WINDOW - Duration::from_millis(1)))];
        assert!(
            Clients::resolve_active(&one_past_window, now).is_none(),
            "one millisecond past the window no longer counts as \"a person is here\""
        );
    }

    #[test]
    fn duplicate_handle_active_resolves_to_one_serial_not_both_rows() {
        // Two connections sharing a handle (a stale reconnect, or a genuine
        // hostname collision) must resolve to exactly one winner BY SERIAL
        // — `is_active_serial` must be true for that one and false for the
        // other, never both (2026-09-08 review, finding 5).
        let clients = Clients::new();
        let a = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-dup".into()));
        let b = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-dup".into()));
        clients.touch_person_input(a.serial());

        let snap = clients.snapshot_with_active();
        assert_eq!(snap.active().map(|x| x.serial), Some(a.serial()));
        assert!(snap.is_active_serial(a.serial()));
        assert!(!snap.is_active_serial(b.serial()), "the untouched duplicate is never active");
    }

    #[test]
    fn receivers_for_matches_by_name_or_unconditionally_by_fe_role() {
        // Roster: a bridge named "X", a frontend named "fe@h" (no bridge,
        // per the module doc — an "fe" row always counts), and the
        // requester itself ("cli", never its own receiver).
        let clients = Clients::new();
        let bridge = clients.register("client-x", "local", None, "0.6.0", 1, "bridge".to_string(), None, None, Some("X".into()));
        let fe = clients.register("client-fe", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@h".into()));
        let me = clients.register("client-me", "local", None, "0.6.0", 1, "cli".to_string(), None, None, Some("self".into()));
        let _ = (&bridge, &fe);

        let mut to_x = clients.receivers_for("X", me.serial());
        to_x.sort();
        assert_eq!(to_x, vec!["X".to_string(), "fe@h".to_string()], "the named match plus the always-on fe row");

        let mut to_absent = clients.receivers_for("Y", me.serial());
        to_absent.sort();
        assert_eq!(to_absent, vec!["fe@h".to_string()], "no name matches, but fe still counts");

        let mut broadcast = clients.receivers_for("", me.serial());
        broadcast.sort();
        assert_eq!(broadcast, vec!["X".to_string(), "fe@h".to_string()], "broadcast is every OTHER named row");

        assert!(
            !clients.receivers_for("self", me.serial()).contains(&"self".to_string()),
            "self_serial is always excluded, even if `to` names the requester's own handle"
        );
    }
}
