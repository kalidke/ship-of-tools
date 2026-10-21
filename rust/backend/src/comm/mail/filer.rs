//! comm.file: file one frame into this daemon's comm folder, or forward it to the hub.

use crate::comm::Env;
use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::CommFileReq;
use sot_protocol::CommFileRes;
use sot_protocol::Frame;
use crate::comm::registry::registry::{iso8601_utc_from_secs, read_registry_fresh, unix_now_secs};
use crate::paths::valid_name;
use crate::server::reply::HandlerOutput;

/// File one frame into THIS daemon's comm folder (`comm.file`, 0031 B1): the
/// hub files for its own home, and a guest on the hub's folder files only on
/// a proven shared lock, forwarding everything else to the hub. A handle this
/// folder does not list is offered to the daemons linked to this one
/// (`Links::offer`), and the answer is theirs; a `not_here` names the boxes that
/// should hold a link and hold none (`hub_link::unlinked`). The response is `{ok:true}` only
/// when the line is in a file; every refusal is `{error, code}` and nothing was
/// appended. The file work runs off the reactor, like the registry helpers.
pub async fn handle_comm_file(
    req_id: u64,
    payload_json: serde_json::Value,
    links: &crate::comm::mail::links::Links,
) -> Result<HandlerOutput> {
    let req: CommFileReq = serde_json::from_value(payload_json).context("comm.file payload")?;
    let (from, to) = (req.from.clone(), req.to.clone());
    // The daemon's entry for this request: everything below reads this value, not the process.
    let env = crate::comm::Env::process();
    let mut verdict = file_comm(env.clone(), req.clone()).await?;
    // A forwarded request has had its one hop; a link's own request is one.
    if !req.forwarded && matches!(&verdict, Err((code, _)) if code == "not_here") {
        let within = crate::comm::mail::inbox::inbox_lock_wait(&env) + COMM_FORWARD_SLACK;
        if let Some(offered) = links.offer(&req, within).await {
            verdict = offered;
        }
        // A box that should hold a link and holds none may hold the handle: the refusal names it.
        if matches!(&verdict, Err((code, _)) if code == "not_here") {
            let attached = links.hosts();
            let unlinked = tokio::task::spawn_blocking(move || crate::comm::mail::hub_link::unlinked(&env, &attached))
                .await
                .context("comm.file topology join")?;
            if !unlinked.is_empty() {
                let text = format!("no linked box knows that handle: {}; not linked: {}", req.to, unlinked.join(", "));
                verdict = Err(("not_here".into(), text));
            }
        }
    }
    let payload = match verdict {
        Ok(()) => {
            tracing::info!(%from, %to, "comm.file filed");
            serde_json::to_value(CommFileRes { ok: true })?
        }
        Err((code, error)) => {
            tracing::info!(%from, %to, code, %error, "comm.file refused");
            json!({ "error": error, "code": code })
        }
    };
    Ok(vec![(Frame::res(req_id, op::COMM_FILE, payload), None)])
}

/// A linked daemon's answer to the hub's offer (`hub_link.rs`): `not_here` unless this box's own folder lists `to`
/// under this box's host, `self_host`; only then do the route, the liveness and the append decide (`file_comm`, the
/// request marked `forwarded`). A box that does not hold the handle never answers with a refusal of its own, so the
/// first refusal the hub reads is the holder's.
pub(crate) async fn file_offer(env: Env, req: CommFileReq, self_host: &str) -> Result<std::result::Result<(), (String, String)>> {
    let (home, to, host) = (crate::comm::sot_comm_home_in(&env), req.to.clone(), self_host.to_string());
    let holds = tokio::task::spawn_blocking(move || home.is_some_and(|h| lists_under(&h.join("registry.json"), &to, &host)))
        .await
        .context("comm.file offer join")?;
    if !holds {
        return Ok(Err(not_here(&req.to)));
    }
    file_comm(env, CommFileReq { forwarded: true, ..req }).await
}

/// Whether the registry at `path` lists `to` under `host`. A registry that cannot be read lists nobody.
fn lists_under(path: &std::path::Path, to: &str, host: &str) -> bool {
    read_registry_fresh(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|root| root["agents"][to]["host"].as_str() == Some(host))
}

/// The refusal for a handle no folder that was asked lists: `(code, sentence)`.
pub(crate) fn not_here(to: &str) -> (String, String) {
    ("not_here".to_string(), format!("no box knows that handle: {to}"))
}

/// The filing behind `comm.file`, callable in-process: the hub link
/// (`hub_link.rs`) files an offer through this same function (`file_offer`).
/// `Err` inside the `Ok` is the refusal `(code, sentence)`.
pub(crate) async fn file_comm(env: Env, req: CommFileReq) -> Result<std::result::Result<(), (String, String)>> {
    tokio::task::spawn_blocking(move || {
        let home = crate::comm::sot_comm_home_in(&env);
        let self_host = comm_self_host_in(&env);
        let topology = sot_protocol::topology::load_in(&env);
        // Recomputed at every filing: a remount changes it under a running daemon.
        let own = home
            .as_deref()
            .map_or_else(|| "none".to_string(), |h| crate::comm::mail::inbox::lock_identity(&h.join("inbox")));
        let own_disk = home.as_deref().is_some_and(|h| crate::comm::mail::inbox::own_disk(&h.join("inbox"), &own));
        let filer = Filer {
            role: if comm_topology_hub(&topology, &self_host) || own_disk {
                crate::comm::mail::inbox::Role::Hub
            } else {
                crate::comm::mail::inbox::Role::Guest
            },
            own,
            machine_id: crate::comm::mail::inbox::machine_id(),
            self_host: self_host.clone(),
        };
        let forward = |fwd: &CommFileReq| {
            let within = crate::comm::mail::inbox::inbox_lock_wait(&env) + COMM_FORWARD_SLACK;
            let endpoint = match &topology {
                Ok(Some((_, t))) => sot_protocol::topology::relay_endpoint(t, &self_host),
                Ok(None) => Err("no hosts.toml names a hub".to_string()),
                Err(e) => return Err(crate::comm::mail::inbox::refusal::hosts_toml_unreadable(e)),
            };
            let endpoint = endpoint.map_err(|e| crate::comm::mail::inbox::refusal::hub_did_not_answer("the relay endpoint", &e))?;
            let payload = serde_json::to_value(fwd).map_err(|e| e.to_string());
            payload.and_then(|payload| crate::topology::dial::dial_and_call(&endpoint, &self_host, sot_protocol::op::COMM_FILE, payload, within)).map_err(|e| {
                let e = e.strip_prefix(&format!("{endpoint}: ")).unwrap_or(&e);
                crate::comm::mail::inbox::refusal::hub_did_not_answer(&endpoint, e)
            })
        };
        comm_file_verdict(
            home.as_deref(),
            &filer,
            forward,
            &req,
            unix_now_secs(),
            crate::comm::mail::inbox::inbox_lock_wait(&env),
        )
    })
    .await
    .context("comm.file join")
}

/// A guest's forward to the hub, and the hub's offer to a linked daemon, answer
/// inside the sender's 5 s at the default lock wait: the other daemon's own lock
/// wait plus this.
const COMM_FORWARD_SLACK: std::time::Duration = std::time::Duration::from_secs(1);

/// This host's name — the one `sotd topology` uses — or empty when it has
/// none, which never matches a hub or a record's writer.
pub(crate) fn comm_self_host() -> String {
    comm_self_host_in(&Env::process())
}

/// [`comm_self_host`] reading `SOT_SELF_HOST` from `env`.
pub(crate) fn comm_self_host_in(env: &Env) -> String {
    sot_log::host::state_dir::host_name_in(env).unwrap_or_default()
}

/// The topology's half of "this daemon is its comm folder's hub": no topology
/// file loads, or it names this host as hub. An unreadable file names no one.
pub(crate) fn comm_topology_hub(
    topology: &std::result::Result<Option<(std::path::PathBuf, sot_protocol::topology::Topology)>, String>,
    self_host: &str,
) -> bool {
    match topology {
        Ok(None) => true,
        Ok(Some((_, t))) => !self_host.is_empty() && t.hub == self_host,
        Err(_) => false,
    }
}

/// Who this daemon is to its comm folder at one filing.
struct Filer {
    role: crate::comm::mail::inbox::Role,
    /// This daemon's own lock manager for `inbox/`.
    own: String,
    /// This machine's id, which a record's line 2 names when it wrote it.
    machine_id: Option<String>,
    self_host: String,
}

/// `comm.file`'s verdict against the comm folder it is handed. In order: `to`
/// is a handle; the route (`comm::mail::inbox::route`) — a forward returns the hub's
/// answer verbatim, a refusal is `file_failed`; this folder LISTS `to`
/// (`.agents[to].host` non-empty — the question `comm-send.sh` and
/// `comm-relay.sh`'s `_registry_target` ask, and deliberately not whether the
/// host is this box); a session holds it (its `last_seen` is fresh: the
/// session stamps it, and the daemon that runs its row, `comm/registry/liveness.rs`);
/// then the append under the inbox lock. `Err` is
/// `(code, sentence)`, the sentence what the sender prints after
/// `FAILED -> @<to>: `.
fn comm_file_verdict(
    comm_home: Option<&std::path::Path>,
    filer: &Filer,
    forward: impl FnOnce(&CommFileReq) -> std::result::Result<serde_json::Value, String>,
    req: &CommFileReq,
    now_secs: u64,
    wait: std::time::Duration,
) -> std::result::Result<(), (String, String)> {
    let to = req.to.as_str();
    if !valid_name(to) {
        return Err(("bad_handle".into(), format!("not a handle: {to:?}")));
    }
    let Some(home) = comm_home else {
        return Err(not_here(to));
    };
    let record_path = home.join(crate::comm::mail::inbox::LOCK_RECORD);
    let record = std::fs::read_to_string(&record_path).ok();
    use crate::comm::mail::inbox::Route;
    let (own, mid) = (filer.own.as_str(), filer.machine_id.as_deref());
    match crate::comm::mail::inbox::route(filer.role, own, mid, record.as_deref(), req.forwarded, &filer.self_host, &record_path) {
        Route::Local => {}
        Route::Forward => {
            let answer = forward(&CommFileReq { forwarded: true, ..req.clone() }).map_err(|e| ("file_failed".to_string(), e))?;
            if answer.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                return Ok(());
            }
            let field = |k: &str| answer.get(k).and_then(|v| v.as_str()).map(str::to_string);
            return Err(match (field("code"), field("error")) {
                (Some(code), Some(error)) => (code, error),
                _ => ("file_failed".into(), format!("the hub answered neither ok nor a refusal: {answer}")),
            });
        }
        Route::Refuse(text) => {
            tracing::error!(%to, "{text}");
            return Err(("file_failed".into(), text));
        }
    }
    file_here(home, &req.from, "daemon", to, req.broadcast, &req.text, now_secs, wait, own)
}

/// The append for a handle this folder lists, once a route has chosen this process (the daemon's `comm.file`, or `sotd
/// comm send` on a box that takes the folder's lock itself): the registry read fresh lists `to`, a session holds it
/// (`last_seen` fresh), then the one append (`inbox::file_frame_from`). `Err` is `(code, sentence)`, the sentence what
/// the sender prints after `FAILED -> @<to>: `; nothing was appended.
#[allow(clippy::too_many_arguments)]
pub(crate) fn file_here(
    home: &std::path::Path,
    from: &str,
    repo: &str,
    to: &str,
    broadcast: bool,
    text: &str,
    now_secs: u64,
    wait: std::time::Duration,
    own: &str,
) -> std::result::Result<(), (String, String)> {
    // A missing registry lists nobody; bytes that are not a registry are no
    // answer at all, never "not here".
    let agents = match read_registry_fresh(&home.join("registry.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_here(to)),
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|root| root.get("agents").filter(|a| a.is_object()).cloned()),
        Err(_) => None,
    };
    let Some(agents) = agents else {
        return Err(("file_failed".into(), format!("the registry could not be read, so @{to} is unverified; nothing was filed")));
    };
    let entry = agents.get(to);
    let field = |k: &str| entry.and_then(|e| e.get(k)).and_then(|v| v.as_str());
    if field("host").map_or(true, str::is_empty) {
        return Err(not_here(to));
    }
    if !heartbeat_fresh(field("last_seen"), now_secs) {
        return Err(("no_live_session".into(), format!("no live session holds @{to}")));
    }
    let ts = iso8601_utc_from_secs(now_secs);
    crate::comm::mail::inbox::file_frame_from(&home.join("inbox"), from, repo, to, broadcast, text, &ts, wait, own)
        .map_err(|e| ("file_failed".into(), e))
}

/// `comm.file`'s liveness: `last_seen` within `LIVE_SECS` of now.
/// Both writers stamp one fixed-width UTC shape (`now_iso`,
/// `iso8601_utc_now`), so string order is time order; a stamp that is absent
/// or not exactly that shape is no heartbeat, never a fresh one.
pub(crate) fn heartbeat_fresh(last_seen: Option<&str>, now_secs: u64) -> bool {
    let Some(s) = last_seen else {
        return false;
    };
    let shape = s.len() == 20
        && s.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        });
    shape && s > iso8601_utc_from_secs(now_secs.saturating_sub(LIVE_SECS)).as_str()
}

/// How old a `last_seen` may be and still count: the one number (0031's ten minutes).
pub(crate) const LIVE_SECS: u64 = 600;

#[cfg(test)]
#[path = "filer_comm_file_tests.rs"]
mod comm_file_tests;
