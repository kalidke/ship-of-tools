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

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::FeCommandEvt;
use sot_protocol::FeCommandSendReq;
use sot_protocol::FeCommandSendRes;
use sot_protocol::Frame;
use tokio::sync::broadcast;

use crate::server::reply::HandlerOutput;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The "active frontend" window: a `last_person_input_at` stamp older than
/// this no longer counts as "a person is here" for `snapshot_with_active`
/// below. Five minutes — long enough that a person reading a preview
/// without touching a key doesn't get silently demoted, short enough that
/// a box the owner walked away from stops absorbing untargeted commands.
const ACTIVE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// One connected frontend, as the backend sees it. `app_version`/`protocol`
/// (ADR 0030 §8 decision 31b) are what `version.query` reports per client —
/// sourced from the hello this connection already sent, never a new probe.
#[derive(Clone, Debug)]
pub struct ClientInfo {
    /// This connection's own registration serial (mirrors the `by_conn`
    /// map key onto the value itself) — carried here so a caller holding
    /// only a `ClientInfo` from a `ClientsSnapshot` can still compare it
    /// against `ClientsSnapshot::active()`'s serial without a second
    /// registry lookup.
    pub serial: u64,
    /// The frontend-supplied, reconnect-stable id (per ADR 0010).
    pub client_id: String,
    /// This client's `HelloReq::app_version`, captured at registration
    /// (first hello on this connection — a later reconnect hello on the
    /// SAME connection keeps the original `register` call).
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
    /// This connection's most recent `fe.sessions` declaration
    /// (session-listing brief decision 2) — the box-wide row list a
    /// frontend's own daemon owns, re-sent edge-driven whenever that list
    /// or any row's state changes. Lives HERE, on the connection, never on
    /// disk: drop the connection and the declaration is gone in the same
    /// breath, no expiry logic needed. `None` until the first `fe.sessions`
    /// arrives (or forever, for a peer that never sends one) — distinct
    /// from `Some(vec![])`, a box that HAS declared and has nothing to
    /// report right now. See `Clients::declare_sessions`.
    pub sessions: Option<Vec<sot_protocol::DeclaredSession>>,
}

/// A hello refused because its declared host has said hello to this daemon as two different OS accounts: two
/// accounts on one computer that share this daemon's hub account would otherwise receive each other's mail (ADR
/// 0049 `## User isolation`). Names the host only, never an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsUserConflict {
    pub host: String,
}

/// The OS accounts one declared host has said hello as, for this daemon's life: one, or several. `Several` is
/// never left until the daemon restarts.
#[derive(Debug, PartialEq, Eq)]
enum HostAccounts {
    One(String),
    Several,
}

#[derive(Default)]
struct Inner {
    /// Keyed by per-connection serial (NOT client_id) so two live
    /// connections from the same machine are distinct entries.
    by_conn: HashMap<u64, ClientInfo>,
    /// Session-listing brief decision 2, final (no-heartbeat) form: box
    /// identity (a declared hello `name`, e.g. `fe@<host>`) → the
    /// `Instant` its connection closed or was reaped — written ONLY for
    /// a connection whose `sessions` was `Some(_)` (it had declared at
    /// least once; see `ClientGuard::drop`), cleared the moment that
    /// same identity declares again (`declare_sessions`). This is the
    /// ONLY thing retained past a disconnect — the sessions themselves
    /// leave with the connection, same as always. In memory only, empty
    /// again on every daemon restart (accepted limit — see `uptime`,
    /// which is what makes that limit visible rather than silent).
    disconnected: HashMap<String, Instant>,
    /// Declared host -> the OS account(s) that said hello for it, live or long gone. In memory only, empty again
    /// on every daemon restart.
    hosts: HashMap<String, HostAccounts>,
}

/// Shared, cheaply-cloneable handle to the connected-client roster.
#[derive(Clone)]
pub struct Clients {
    inner: Arc<Mutex<Inner>>,
    next_serial: Arc<AtomicU64>,
    /// This daemon PROCESS's own start time — session-listing brief: the
    /// header line names how far back the `disconnected` map's memory
    /// reaches ("this daemon, up 3h"), so a restart's forgetting is
    /// visible rather than read as "no sessions". `Clients::new()` is
    /// called once per daemon process, so this is set once, at boot.
    started_at: Instant,
}

impl Clients {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            next_serial: Arc::new(AtomicU64::new(1)),
            started_at: Instant::now(),
        }
    }

    /// How long this daemon process has been up — the session-listing
    /// brief's header value, and nothing else: no other decision reads
    /// this.
    pub fn uptime(&self) -> Duration {
        Instant::now().saturating_duration_since(self.started_at)
    }

    /// Admits `host`'s hello as `os_user`, or refuses it: the first account to say hello for a host is remembered;
    /// the first time another account does, the host becomes `Several` and that hello is refused, and so is every
    /// later hello for the host, whichever account, until the daemon restarts. Connections already registered for
    /// the host are left alone. The check and the record are one step under the roster's lock, so two accounts
    /// connecting at the same instant cannot both pass.
    pub fn admit_account(&self, host: &str, os_user: &str) -> Result<(), OsUserConflict> {
        let mut g = self.inner.lock().unwrap();
        match g.hosts.get(host) {
            None => {
                g.hosts.insert(host.to_string(), HostAccounts::One(os_user.to_string()));
                Ok(())
            }
            Some(HostAccounts::One(first)) if first == os_user => Ok(()),
            Some(HostAccounts::One(_)) => {
                g.hosts.insert(host.to_string(), HostAccounts::Several);
                Err(OsUserConflict { host: host.to_string() })
            }
            Some(HostAccounts::Several) => Err(OsUserConflict { host: host.to_string() }),
        }
    }

    /// Register a connection. Returns a guard that deregisters on drop —
    /// hold it for the connection's lifetime. Logs the new live count and
    /// the distinct `client_id`s currently attached.
    pub fn register(
        &self,
        client_id: impl Into<String>,
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
            app_version: app_version.into(),
            protocol,
            role,
            host,
            instance,
            name,
            last_person_input_at: None,
            sessions: None,
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

    /// Store the connection at `serial`'s latest `fe.sessions` declaration
    /// (session-listing brief decision 2). Lives on the connection, same
    /// lifetime as `last_person_input_at` — dropping the connection drops
    /// this too, which IS the "dropped connection / stale declaration"
    /// rule: no expiry, no reconciliation, nothing to go stale. A no-op if
    /// `serial` isn't registered (already disconnected, or `hello` hasn't
    /// landed yet), mirroring `touch_person_input`.
    pub fn declare_sessions(&self, serial: u64, sessions: Vec<sot_protocol::DeclaredSession>) {
        let mut g = self.inner.lock().unwrap();
        let name = match g.by_conn.get_mut(&serial) {
            Some(info) => {
                info.sessions = Some(sessions);
                info.name.clone()
            }
            None => return,
        };
        // A box that just declared again is, by definition, not missing
        // — clear whatever "not connected since" entry it left behind
        // last time it dropped (borrow of `by_conn` above must end
        // first: `disconnected` is a sibling field, not the same one).
        if let Some(name) = name {
            g.disconnected.remove(&name);
        }
    }

    /// This connection's declared hello `name`, read with its own lock
    /// acquisition — `handle_fe_sessions` checks this BEFORE calling
    /// `declare_sessions`, because an unnamed declarer cannot be
    /// attributed to a box (session-listing brief): refused, not
    /// stored. `None` for an unregistered serial too, same "nothing to
    /// report" convention as every other not-found case here.
    pub fn name_for(&self, serial: u64) -> Option<String> {
        self.inner.lock().unwrap().by_conn.get(&serial).and_then(|c| c.name.clone())
    }

    /// Every box identity this daemon currently has a "not connected
    /// since" entry for for, each paired with how long ago (computed
    /// from the SAME `now` the caller passes, so a `version.query` that
    /// reads this alongside `snapshot_with_active` never disagrees with
    /// itself about what "now" meant). Session-listing brief decision
    /// 2/amendment 2 — the only state retained past a disconnect.
    pub fn disconnected_since(&self, now: Instant) -> Vec<(String, Duration)> {
        self.inner
            .lock()
            .unwrap()
            .disconnected
            .iter()
            .map(|(identity, at)| (identity.clone(), now.saturating_duration_since(*at)))
            .collect()
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
            let departed = g.by_conn.remove(&self.serial);
            // Session-listing brief decision 2 (final, no-heartbeat
            // form): a box goes into `disconnected` ONLY if it had
            // declared at least once (`sessions: Some(_)`) — a
            // connection that never sent `fe.sessions` (a bridge/cli/
            // agent, or an old frontend) has nothing to be missed FOR,
            // so it leaves no trace here, exactly as before this
            // feature existed. `remove` above already ended the
            // `by_conn` borrow, so touching `disconnected` next is a
            // second, disjoint mutation of `g`, not a re-borrow.
            if let Some(info) = departed {
                if info.sessions.is_some() {
                    if let Some(name) = info.name {
                        if !name.is_empty() {
                            g.disconnected.insert(name, Instant::now());
                        }
                    }
                }
            }
            (g.by_conn.len(), distinct_client_ids(&g.by_conn))
        };
        // Reap this connection's docs.open site root (ADR 0029). The map is keyed
        // by serial, so this drops exactly the departing connection's entry — the
        // only cleanup site needed (the ADR-0027 reaper guarantees we reach Drop
        // even for half-open / hung peers, so this inherits its coverage).
        crate::pages::site::remove_root(self.serial);
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

/// `version.query` (ADR 0030 §8 decision 31b, ADR 0043 decision 31): pure
/// in-memory, no fan-out, no supervisor probe — this daemon's own version
/// triple plus the roster of currently-attached frontends, sourced from
/// their hellos. Never fails: an empty `clients` list from a daemon with
/// zero OTHER attached frontends is a legitimate answer, not an error.
///
/// Each row also carries the connection's declared `host`/`role`/`instance`/
/// `name` (ADR 0046 decision 1) plus `active`: ONE `snapshot_with_active()`
/// call supplies both the roster and the winner from the same lock + the
/// same `now` (2026-09-08 review, finding 6 — two separate reads could
/// disagree under concurrent `fe.presence` traffic), and `active` is set by
/// comparing SERIALS, never handles, so a duplicate-handle connection that
/// isn't the winner is never marked active alongside it (finding 5).
///
/// Topology plan §B "on-demand re-read": this call also re-reads this
/// daemon's `hosts.toml` (`TopologyStore::refresh`) so `hosts_toml_hash`
/// is never stale by more than one hand edit, and — same as `topology.set`
/// — broadcasts `topology.changed` if that re-read picked up a change
/// nothing else had already announced. Never a file watcher (the file can
/// be on a network filesystem); this op and `topology.*` are the only
/// re-read triggers.
pub async fn handle_version_query(
    req_id: u64,
    clients: &crate::clients::Clients,
    topology: &crate::topology::store::TopologyStore,
    topo_tx: &broadcast::Sender<crate::topology::store::TopologyChanged>,
) -> Result<HandlerOutput> {
    let refreshed = topology.refresh();
    if refreshed.changed {
        let _ = topo_tx.send(crate::topology::store::TopologyChanged {
            hash: refreshed.hash.clone().unwrap_or_default(),
        });
    }
    let daemon = sot_protocol::DaemonVersion {
        app_version: sot_protocol::app_version(),
        protocol: sot_protocol::PROTOCOL_VERSION,
        lane_build: sot_log::identity::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
        lane_proto: sot_log::lane::wire::SUPERVISOR_PROTO_V1,
        host: crate::rows::store::declared_host(),
        hosts_toml_hash: refreshed.hash.unwrap_or_default(),
        uptime_s: clients.uptime().as_secs(),
    };
    let snap = clients.snapshot_with_active();
    let client_rows = snap
        .clients
        .iter()
        .map(|c| sot_protocol::ClientVersion {
            client_id: c.client_id.clone(),
            app_version: c.app_version.clone(),
            protocol: c.protocol,
            host: c.host.clone(),
            role: c.role.clone(),
            instance: c.instance.clone(),
            name: c.name.clone(),
            active: snap.is_active_serial(c.serial),
            sessions: c.sessions.clone(),
        })
        .collect();
    // Session-listing brief decision 2 / amendment 2: every box whose
    // connection has since closed or been reaped, read at the SAME
    // convention `Instant::now()` — `disconnected` lives outside
    // `by_conn`, so this needs its own read regardless of `snap` above.
    // Review blocker 1: `name` identifies a BOX, not a process (a second
    // frontend on the same box exiting still leaves that box's `name`
    // attached elsewhere), so an identity ALSO held by a still-connected
    // row right now must not be reported disconnected — filtered against
    // THIS SAME `snap`, never a second registry read.
    let disconnected = clients
        .disconnected_since(std::time::Instant::now())
        .into_iter()
        .filter(|(identity, _)| !snap.clients.iter().any(|c| c.name.as_deref() == Some(identity.as_str())))
        .map(|(identity, age)| sot_protocol::DisconnectedBox {
            identity,
            since_s: age.as_secs(),
        })
        .collect();
    let res = sot_protocol::VersionQueryRes {
        daemon,
        clients: client_rows,
        disconnected,
    };
    Ok(vec![(
        Frame::res(req_id, op::VERSION_QUERY, serde_json::to_value(res)?),
        None,
    )])
}

/// `fe.command.send` (ADR 0025): parse the imperative UI command, build an
/// `FeCommandEvt { v:1, cmd, args, target }`, publish it onto the FE-command
/// broadcast channel (each connection turns it into an `fe.command` evt), and
/// ack. Structurally mirrors `handle_agent_send`.
///
/// 2026-09-08 review rework — an untargeted request (`target: None`) is
/// resolved HERE, before publish, from ONE `Clients::snapshot_with_active()`
/// call:
/// - An active frontend exists: deliver to it EXCLUSIVELY, by connection
///   serial (design point B — a bare handle string is not a reliable
///   identity; `evt.target_serial` carries the serial for `server/events.rs`'s
///   per-connection fan-out to filter on, while `evt.target` still carries
///   the handle for the FE's own — now redundant but harmless —
///   `route_fe_command` self-check).
/// - No active frontend, and `cmd == "relaunch"`: publish NOTHING (design
///   point E — an unresolved relaunch broadcast is a command nobody can
///   safely execute; every FE refuses an undirected one anyway, so the
///   daemon not sending it is strictly more honest, not less capable).
/// - No active frontend, any other `cmd`: fall through to the pre-existing
///   broadcast (`target` stays `None`, `target_serial` stays `None` — every
///   connection's `route_fe_command` self-filter sees the badge floor,
///   unchanged from before this design).
///
/// An explicit `target` from the caller is never touched, and delivery for
/// it stays a handle-matched broadcast exactly as before (`target_serial`
/// stays `None`, so every matching connection self-filters as today).
///
/// `resolved_target` on the ack (design point E) always mirrors the final
/// `target` — `None` when nothing was resolved (whether or not something
/// broadcast), so a caller like `sot-fe relaunch` can tell "no active
/// frontend" apart from "delivered/broadcast" without inspecting `cmd`
/// itself.
///
/// `delivered_to` (2026-09-09 field incident: a broadcast `open-url` acked
/// `ok:true` twice while landing on a machine other than the one the owner
/// was sitting at) is the count of ATTACHED FRONTENDS this command was
/// actually published to, read off the SAME `snapshot_with_active()` call
/// `resolved_target` was resolved from — never a second lock acquisition
/// (`handle_version_query` sets the precedent). It counts what will
/// ACTUALLY act, which is not always a handle count: a target resolved to
/// the active frontend is delivered by SERIAL, so it is exactly 1 even when
/// a second connection shares that handle; an explicit `--fe <handle>` stays
/// a handle-matched broadcast, so its audience IS the handle count (0 when
/// nothing carries it — the incident above); `target: None` counts every row
/// with a non-empty declared `name` (the badge-floor broadcast's audience); the
/// unresolved-relaunch early return below is `Some(0)` — it published
/// nothing. This never changes WHAT gets published, only what the ack
/// truthfully reports about it.
pub async fn handle_fe_command_send(
    req_id: u64,
    payload_json: serde_json::Value,
    fe_tx: &broadcast::Sender<FeCommandEvt>,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let mut req: FeCommandSendReq =
        serde_json::from_value(payload_json).context("fe.command.send payload")?;
    let mut target_serial: Option<u64> = None;
    let snap = clients.snapshot_with_active();
    if req.target.is_none() {
        if let Some(active) = snap.active() {
            req.target = Some(active.handle.clone());
            target_serial = Some(active.serial);
        }
    }
    let resolved_target = req.target.clone();
    // Relay diagnostics include the optional forwarded workspace and path without changing command routing.
    let forwarded = |key: &str| req.args.get(key).and_then(serde_json::Value::as_str);
    let (workspace, path) = (forwarded("ws"), forwarded("path"));

    if req.cmd == "relaunch" && req.target.is_none() {
        tracing::info!(
            cmd = %req.cmd, workspace, path,
            delivered_to = 0,
            "fe.command.send relay: relaunch has no active frontend and no explicit target — not publishing"
        );
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FE_COMMAND_SEND,
                serde_json::to_value(FeCommandSendRes {
                    ok: true,
                    resolved_target,
                    delivered_to: Some(0),
                })?,
            ),
            None,
        )]);
    }

    let delivered_to = match (target_serial, resolved_target.as_deref()) {
        // Resolved to the ACTIVE frontend: `server/events.rs` fans out on the
        // SERIAL, so exactly that one CONNECTION acts — however many
        // connections happen to share its handle (a relaunched frontend
        // whose predecessor's connection has not been reaped yet is the
        // real case). Counting handles here would over-report the audience
        // of an exclusive delivery: the same lie in miniature that this
        // field exists to end.
        (Some(_), _) => 1,
        // An explicit `--fe <host>` (`target = fe@<host>`) stays a
        // name-matched broadcast — every frontend carrying that name
        // self-filters as a match, so the name count IS the audience.
        // Gated on `role`: a bridge/cli/agent's `name` never inflates a
        // frontend-targeted count, even on a coincidental string match.
        (None, Some(handle)) => snap
            .clients
            .iter()
            .filter(|c| c.role == "fe" && c.name.as_deref() == Some(handle))
            .count(),
        // The badge floor: every attached, named frontend acts.
        (None, None) => snap
            .clients
            .iter()
            .filter(|c| c.role == "fe" && c.name.as_deref().is_some_and(|h| !h.is_empty()))
            .count(),
    };

    tracing::info!(cmd = %req.cmd, workspace, path, target = ?req.target, delivered_to, "fe.command.send relay");
    let evt = FeCommandEvt {
        v: 1,
        cmd: req.cmd,
        args: req.args,
        target: req.target,
        target_serial,
    };
    // Fire-and-forget broadcast; send error means no subscribers, harmless.
    let _ = fe_tx.send(evt);
    Ok(vec![(
        Frame::res(
            req_id,
            op::FE_COMMAND_SEND,
            serde_json::to_value(FeCommandSendRes {
                ok: true,
                resolved_target,
                delivered_to: Some(delivered_to),
            })?,
        ),
        None,
    )])
}

/// `fe.presence` (2026-09-08 review rework, design point A): a person
/// provided real input; ack only. Stamping happens in `server/dispatch.rs`'s
/// dispatch (it needs this connection's registered serial, which isn't
/// visible from an op payload alone).
pub async fn handle_fe_presence(req_id: u64) -> Result<HandlerOutput> {
    Ok(vec![(
        Frame::res(
            req_id,
            op::FE_PRESENCE,
            serde_json::to_value(sot_protocol::FePresenceRes { ok: true })?,
        ),
        None,
    )])
}

/// `fe.sessions` (session-listing brief decision 2): a frontend declares
/// the sot-comm handles its own box's daemon owns, so THIS daemon can list
/// them. Unlike `fe.presence` (which needs no payload and stamps via
/// `server/dispatch.rs`'s dispatch loop, since the thing being stamped is the
/// connection itself), the store happens here — the payload IS what's
/// stored. An unregistered `serial` (`None`) is a harmless
/// no-op ack, same as `touch_person_input`; a registered connection with
/// NO declared hello `name` is refused (`unnamed_connection`) rather than
/// stored — the `disconnected` map keys on that name, so a declaration
/// with none could never be attributed to a box once its connection
/// dropped. `declare_sessions` also clears that box's `disconnected`
/// entry, if it had one — a box that just declared again is, by
/// definition, not missing.
pub async fn handle_fe_sessions(
    req_id: u64,
    payload_json: serde_json::Value,
    clients: &crate::clients::Clients,
    serial: Option<u64>,
) -> Result<HandlerOutput> {
    let req: sot_protocol::FeSessionsReq =
        serde_json::from_value(payload_json).context("fe.sessions payload")?;
    let Some(serial) = serial else {
        // No roster entry: the same harmless
        // no-op `touch_person_input` accepts, since there is nothing to
        // store OR refuse against.
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FE_SESSIONS,
                serde_json::to_value(sot_protocol::FeSessionsRes { ok: true })?,
            ),
            None,
        )]);
    };
    // An unnamed connection is refused, not stored (session-listing
    // brief): the `disconnected` map's identity IS the declared hello
    // `name`, so a declarer with none could never be attributed to a
    // box if its connection later dropped — better to refuse now than
    // store a declaration that can never resurface as anything.
    match clients.name_for(serial) {
        Some(name) if !name.is_empty() => {
            clients.declare_sessions(serial, req.sessions);
            Ok(vec![(
                Frame::res(
                    req_id,
                    op::FE_SESSIONS,
                    serde_json::to_value(sot_protocol::FeSessionsRes { ok: true })?,
                ),
                None,
            )])
        }
        _ => Ok(vec![(
            Frame::res(
                req_id,
                op::FE_SESSIONS,
                json!({
                    "error": "this connection declared no name at hello, so its sessions cannot be attributed to a box",
                    "code": "unnamed_connection",
                }),
            ),
            None,
        )]),
    }
}

#[cfg(test)]
#[path = "clients_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "clients_relay_tests.rs"]
mod relay_tests;
