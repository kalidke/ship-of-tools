// the connection and the daemon's client roster: hello, ping, version.query, fe.command, fe.presence, fe.sessions, topology.set

use super::*;

/// Connect handshake. Per ADR 0010, every connect carries
/// `(session_id, client_id, last_seen_revision)`. First-time connect leaves
/// `session_id` as None and accepts whatever the backend assigns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReq {
    pub client_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub last_seen_revision: u64,
    /// App-level token. Backend resolution order: `--token` (compat) else
    /// `$SOT_TOKEN` else the canonical `${XDG_CONFIG_HOME:-~/.config}/sot/token`
    /// file (a single value, not a `tokens.toml` — that path is stale).
    /// Required on TCP transport whenever the backend has one configured.
    /// Local Unix-socket transport relies on SSH user identity and private
    /// filesystem permissions instead of an app token.
    #[serde(default)]
    pub token: Option<String>,
    /// Wire-contract protocol version the client speaks (ADR 0030 §2). The
    /// backend gates the handshake on integer equality against its own
    /// `PROTOCOL_VERSION`. `#[serde(default)]` → `0` for a pre-versioning peer
    /// that predates this field; `0` is treated as "pre-versioning" and gets a
    /// one-time transition grace while the backend's own version is still 1.
    #[serde(default)]
    pub protocol: u32,
    /// The client's product version string (`sot_protocol::app_version()`),
    /// e.g. `0.1.0-dev+abc`. `#[serde(default)]` → `""` for a pre-versioning
    /// peer. Reported back verbatim in a protocol-mismatch error so the user
    /// sees both sides' versions.
    #[serde(default)]
    pub app_version: String,
    /// This connection's declared host (ADR 0046 decision 1) —
    /// `sot_log::host::state_dir::host_name()`'s own value, self-reported by
    /// every client kind, not only frontends. `#[serde(default)]` → `None`
    /// for a pre-this-field peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// This connection's declared role: `"fe"` a frontend, `"bridge"` a
    /// session's listener loop, `"cli"` a one-shot call from a shell,
    /// `"agent"` a one-shot call from inside a session (ADR 0046 decision
    /// 1). Required since protocol 2: an empty role is never `"fe"`.
    #[serde(default)]
    pub role: String,
    /// Opaque per-process instance discriminator (ADR 0046 decision 1) —
    /// the frontend's own `FrontendIdentity::instance`, minted once per
    /// process rather than resampled per connection. `None` for a role
    /// with no notion of instance, or a pre-this-field peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// This connection's own declared name, the address a directed
    /// `fe.command.send` `target` matches: a frontend declares `fe@<host>`
    /// (`--fe <host>` on the shell side; `instance` alone splits two
    /// frontends on one box), a bridge/cli/agent its sot-comm handle.
    /// Protocol 2 folded the old frontend-only `fe_handle` into this one
    /// field for every role. `None` for a role that declares nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloRes {
    pub session_id: String,
    pub revision: u64,
    /// True if the backend will follow up with a snapshot because the client
    /// fell off the back of the event ring. False means the client is caught
    /// up enough to take it from here without help.
    pub snapshot_pending: bool,
    /// Backend-reported hostname (`gethostname`). Lets the chrome show
    /// "connected to myhost" instead of just a session id. Optional so an
    /// older backend that doesn't fill it still works — frontend falls
    /// back to the CLI transport target in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The directory the backend is serving (`--project-root`), so the
    /// chrome can show "myhost:Ship of Tools" rather than just the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Optional human-friendly label passed to the backend via `--label`
    /// (or the env var). Sessions mode uses this to match the running
    /// daemon to its `~/.config/sot/sessions/<id>.toml` entry per ADR
    /// 0013. Absent when the backend was launched the old way without a
    /// label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many frontend connections are live on this backend *including*
    /// the one this hello answers (ADR 0010/0013 multi-frontend). 1 in the
    /// normal single-FE case; >1 when the user has another device attached.
    /// `#[serde(default)]` so an older backend that omits it deserializes
    /// to 0 on a newer frontend.
    #[serde(default)]
    pub clients_connected: usize,
    /// The backend's wire-contract protocol version (ADR 0030 §2), mirror of
    /// `HelloReq::protocol`. `#[serde(default)]` → `0` for a pre-versioning
    /// backend that predates the field; the frontend `tracing::warn!`s when a
    /// successful hello comes back with `0` (legacy backend) but still runs.
    #[serde(default)]
    pub protocol: u32,
    /// The backend's product version string (`sot_protocol::app_version()`).
    /// `#[serde(default)]` → `""` for a pre-versioning backend.
    #[serde(default)]
    pub app_version: String,
    /// True when this backend accepts `proxy.connect` connections (ADR
    /// 0035). The frontend arms its lazy loopback proxy listeners only when
    /// set; `#[serde(default)]` → `false` from an older backend, in which
    /// case the FE relies on the legacy launcher `-L` forwards exactly as
    /// before.
    #[serde(default)]
    pub proxy: bool,
}

/// `fe.command.send` request (ADR 0025) — ask the daemon to drive the
/// frontend(s) with an imperative UI command. The daemon re-emits
/// `{v:1, cmd, args, target}` as an `FE_COMMAND` evt (mirrors `AGENT_SEND` →
/// `AGENT_MESSAGE`). `cmd` ∈ {"preview", "reveal", "goto_workspace",
/// "goto_mode", "notify"} for v1; `args` is the per-cmd object (e.g.
/// `preview` = `{workspace, path, urgent?}`, `goto_workspace` = `{workspace}`).
/// `target` optionally scopes delivery to one FE by its sot-comm handle;
/// absent = the daemon resolves the active frontend (2026-09-08 review
/// rework) and delivers to that ONE connection exclusively, falling back to
/// every FE (the badge floor) only when none is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandSendReq {
    pub cmd: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `fe.command.send` response — a bare ack plus `resolved_target` (2026-09-08
/// review rework, design point E): the handle the daemon actually resolved
/// `target` to, or `None` if it published unresolved (a genuine broadcast) or
/// didn't publish at all. `ok` is true on a parsed request; the FE-command
/// publish is fire-and-forget, so a send with no FE connected still acks ok
/// (mirrors `AgentSendRes`). `#[serde(default)]` on `resolved_target` so an
/// old daemon that predates this field deserializes to `None` here too —
/// `sot-fe` treats "old daemon" and "no active frontend" identically for
/// `relaunch`: both mean "we don't know this reached anyone," so it exits
/// non-zero with a `--fe` hint either way rather than reporting success.
///
/// `delivered_to` (2026-09-09 field incident: a broadcast `open-url` acked
/// `ok:true` twice while landing on a machine other than the one the owner
/// was sitting at) is how many ATTACHED FRONTENDS this command was actually
/// published to. `Some(0)` means nothing can act on it — an explicit
/// `--fe <handle>` that matched no attached connection, no frontend attached
/// at all, or an undirected `relaunch` the daemon deliberately refused to
/// publish (design point E above). `None` means this daemon predates the
/// field and cannot say — NOT "zero"; `#[serde(default)]` gives that
/// deserialization for free.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandSendRes {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_to: Option<usize>,
}

/// Payload of an `FE_COMMAND` evt (ADR 0025) — one imperative UI command pushed
/// to the frontend(s). `v` is the envelope version (1). The FE parses `{cmd, args}`
/// into an `FeCommand` and dispatches it through `dispatch_fe_command`; `target`
/// (a FE sot-comm handle) scopes which FE acts (`None` = all, the badge floor;
/// `Some` = only that FE, force-show routing). The FE self-filters on `target`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeCommandEvt {
    #[serde(default = "fe_command_version")]
    pub v: u32,
    pub cmd: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// DAEMON-INTERNAL ONLY — `#[serde(skip)]`, never reaches the wire in
    /// either direction. The connection SERIAL the daemon resolved via
    /// `Clients::snapshot_with_active` when it auto-targeted this event
    /// (2026-09-08 review rework, design point B: a handle string is not a
    /// reliable identity — two connections can share one, e.g. a stale
    /// reconnect). `server/events.rs`'s per-connection fan-out drops this event
    /// for every connection whose own serial doesn't match, so exactly one
    /// connection ever writes it to its wire — `target` above still rides
    /// along for that one connection's own (redundant, harmless)
    /// `route_fe_command` self-check. `None` here means every connection
    /// may act on `target` as today: a handle-matched broadcast for an
    /// explicit `--fe <handle>`, or an unresolved genuine broadcast.
    #[serde(skip)]
    pub target_serial: Option<u64>,
}

fn fe_command_version() -> u32 {
    1
}

/// `fe.presence` request (2026-09-08 review rework, design point A) — empty:
/// the frontend's own signal that a PERSON just provided real keyboard or
/// mouse input, sent from the winit input handlers themselves (throttled
/// there), never inferred by the daemon from other op traffic. See
/// `Clients::touch_person_input`, which this op alone drives.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FePresenceReq {}

/// `fe.presence` response — a bare ack; the frontend doesn't act on it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FePresenceRes {
    pub ok: bool,
}

/// One sot-comm handle a frontend's own box files for, as `fe.sessions`
/// declares it (session-listing brief decision 2) — the same four fields
/// `workspace.list`'s `WorkspaceInfo` already carries per row
/// (`agent_handle`, `agent_state`, `agent_summary`, `agent_status_at`),
/// copied through verbatim. No second source of truth: the hub derives
/// nothing beyond what the declaring box's own daemon already computed,
/// so the hub can never be MORE wrong than the strip the person on that
/// box sees. Only rows with a non-empty `agent_handle` are ever declared
/// (a row that never joined names nobody — ADR 0046); `handle` here is
/// therefore never empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredSession {
    pub handle: String,
    pub state: String,
    pub summary: String,
    pub status_at: String,
}

/// `fe.sessions` request (session-listing brief decision 2): the sending
/// box's complete row list, re-sent whenever it changes — a new/closed row
/// or any row's state change re-declares, edge-driven, no timer, no
/// heartbeat. Carries every declared row, not a diff, so the daemon never
/// reconstructs a set from a series of edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeSessionsReq {
    pub sessions: Vec<DeclaredSession>,
}

/// `fe.sessions` response — a bare ack, mirroring `fe.presence`; the
/// frontend doesn't act on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeSessionsRes {
    pub ok: bool,
}

/// `ping` request — empty, always (topology plan §F step 2). Distinct op
/// from `fe.presence`: presence means "a person is here", ping means
/// "this connection's read half is alive" — conflating them was the
/// rejected alternative (D10).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PingReq {}

/// `ping` response — a bare ack; the sender doesn't act on it beyond
/// having received a reply at all.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PingRes {
    pub ok: bool,
}

/// `version.query` request — empty, always (ADR 0030 §8 decision 31b).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionQueryReq {}

/// This daemon's own version triple, as `version.query` reports it.
/// `lane_proto` is the supervisor-lane wire protocol integer this daemon
/// gates on (ADR 0045 decision 7: `wire::SUPERVISOR_PROTO_V1`, compared
/// against any supervisor it attaches, adopts, or spawns); `lane_build`
/// is `sot_log::identity::exchange::SUPERVISOR_LANE_BUILD_ID`, carried alongside
/// as an informational build stamp only — never compared. Both are
/// distinct from `app_version`, which is the product version (ADR 0030
/// §1) and never gates a capsule attach. `#[serde(default)]` on
/// `lane_proto`: a daemon predating ADR 0045 answers without it.
///
/// `host` (topology plan §F step 1) is this daemon's own declared host —
/// `rows::store::declared_host()` on the backend side, the same
/// `sot_log::host::state_dir::host_name()` resolution ADR 0046 uses for
/// `HelloRes.host` and `ClientVersion.host`. `#[serde(default)]`: a
/// daemon predating this field answers without it and an old client
/// parsing a new daemon's extra field is unaffected either way — this is
/// what lets `sot-fe version` answer "which host is this daemon on"
/// without reading a log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonVersion {
    pub app_version: String,
    pub protocol: u32,
    pub lane_build: String,
    #[serde(default)]
    pub lane_proto: u32,
    #[serde(default)]
    pub host: String,
    /// `hash_text` of this daemon's currently-loaded `hosts.toml` (plan §B
    /// "the CLI... compare this box's file hash with the hub's"), as of
    /// the on-demand re-read this `version.query` call itself triggers —
    /// never stale by more than one hand edit. `""` when this daemon has
    /// no `hosts.toml` at all (a fresh box with no topology declared) OR
    /// predates this field (`#[serde(default)]`); a caller must not treat
    /// `""` as "matches mine" for the "cache diverged" comparison.
    #[serde(default)]
    pub hosts_toml_hash: String,
    /// How long this daemon PROCESS has been up (`Clients::uptime`,
    /// session-listing brief) — what a "not connected since" line's
    /// reader needs to see how far back this daemon's memory of
    /// disconnected boxes actually reaches: a restart forgets every one
    /// of them, and this is what makes that forgetting visible instead
    /// of silently read as "no sessions". `#[serde(default)]` → `0` for
    /// a daemon that predates this field — the header degrades to
    /// printing no uptime rather than failing to parse.
    #[serde(default)]
    pub uptime_s: u64,
}

/// One box this daemon has heard `fe.sessions` from whose connection has
/// since closed or been reaped (session-listing brief decision 2,
/// amendment 2's final no-heartbeat form) — the ONLY thing retained past
/// a disconnect; the sessions themselves left with the connection. A
/// SEPARATE list from `ClientVersion`, not optional fields on that type:
/// a disconnected box has no sessions by definition, and a shape that
/// could express one would invite printing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisconnectedBox {
    /// The declared hello `name` this box's frontend connection last
    /// carried (e.g. `fe@<host>`) — the same identity a `ClientVersion`
    /// row would have shown while it was still attached.
    pub identity: String,
    /// Seconds since that connection closed or was reaped, computed by
    /// the daemon from its own monotonic clock at THIS `version.query`
    /// call — never a wall-clock timestamp (a box in another timezone
    /// would mislead) and never re-derived by the caller from a stored
    /// instant it has no way to read.
    pub since_s: u64,
}

/// One attached frontend, as `version.query` reports it — sourced from the
/// hello that connection already sent (`HelloReq::app_version`/`protocol`),
/// never a new probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientVersion {
    pub client_id: String,
    pub app_version: String,
    pub protocol: u32,
    /// This client's self-reported `HelloReq::host` (ADR 0046 decision 1)
    /// — `None` for a peer that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// This client's self-reported `HelloReq::role`.
    #[serde(default)]
    pub role: String,
    /// This client's self-reported `HelloReq::instance` — `None` for a
    /// role with no notion of instance, or a peer that predates the
    /// field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// This client's self-reported `HelloReq::name` — `fe@<host>` for a
    /// frontend, a sot-comm handle for a bridge/cli/agent, `None` for a
    /// role that declared nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// True for the one connection (at most, identified by SERIAL, never
    /// merely by a matching handle — see `Clients::snapshot_with_active`)
    /// that an untargeted `fe.command.send` would be delivered to right
    /// now. No idle age is reported here (deleted 2026-09-08 review,
    /// finding 6): deriving "how idle" in shell would duplicate the
    /// daemon's own expiry/tie policy, and a caller only ever needs to
    /// know WHICH one is active, never by how much. `#[serde(default)]` →
    /// `false` for a daemon that predates this field.
    #[serde(default)]
    pub active: bool,
    /// This connection's most recent `fe.sessions` declaration
    /// (session-listing brief decision 2) — `None` for a peer that has
    /// never sent one (an old frontend, or a frontend box running no
    /// daemon at all): "declares no sessions". Distinct from
    /// `Some(vec![])`, a box that HAS declared and currently has no
    /// sessions to report: "no sessions". `#[serde(default,
    /// skip_serializing_if = "Option::is_none")]` matches `host`/`name`
    /// above — omitted on the wire, not `null`, for a peer with nothing
    /// to say, so the two absences (predates the field vs. never
    /// declared) read the same on a caller that doesn't care to tell
    /// them apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<DeclaredSession>>,
}

/// `version.query` response (ADR 0030 §8 decision 31b, ADR 0043 decision
/// 31). Deliberately carries no per-capsule-row build info: `workspace.list`
/// already fans out to every supervisor per call, and its `phase` field
/// (`"foreign"`, decision 31c) already says everything an operator acts on
/// differently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionQueryRes {
    pub daemon: DaemonVersion,
    /// `#[serde(default)]`: additive, mirrors every other collection field
    /// in this protocol (`HelloRes` legacy tolerance below) — a peer that
    /// answers this op but omits the roster still deserializes to no
    /// clients rather than failing the whole response.
    #[serde(default)]
    pub clients: Vec<ClientVersion>,
    /// Every box this daemon has heard `fe.sessions` from that isn't
    /// attached right now (session-listing brief) — `#[serde(default)]`,
    /// same additive-tolerance idiom as `clients` above.
    #[serde(default)]
    pub disconnected: Vec<DisconnectedBox>,
}

/// `topology.set` request (plan §B "Editing the master list") — one edit,
/// applied atomically. See [`op::TOPOLOGY_SET`] for the full authority/
/// refusal contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologySetReq {
    pub edit: crate::topology::TopologyEdit,
}

/// `topology.set` response on success. `hash` is the new file's
/// `hash_text` — the same value [`op::TOPOLOGY_CHANGED`] broadcasts and
/// `version.query`'s `hosts_toml_hash` reports, so a caller can confirm
/// its own edit landed without a follow-up query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologySetRes {
    pub ok: bool,
    pub hash: String,
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
