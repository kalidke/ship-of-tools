//! comm.file: file one frame into this daemon's comm folder, or forward it to the hub.

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
/// a proven shared lock, forwarding everything else to the hub. The response
/// is `{ok:true}` only when the line is in the file; every refusal is
/// `{error, code}` and nothing was appended. The file work runs off the
/// reactor, like the registry helpers.
pub async fn handle_comm_file(req_id: u64, payload_json: serde_json::Value) -> Result<HandlerOutput> {
    let req: CommFileReq = serde_json::from_value(payload_json).context("comm.file payload")?;
    let (from, to) = (req.from.clone(), req.to.clone());
    let verdict = file_comm(req).await?;
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

/// The filing behind `comm.file`, callable in-process: the hub link
/// (`hub_link.rs`) files what arrives on its connection through this same
/// function. `Err` inside the `Ok` is the refusal `(code, sentence)`.
pub(crate) async fn file_comm(req: CommFileReq) -> Result<std::result::Result<(), (String, String)>> {
    tokio::task::spawn_blocking(move || {
        let home = crate::comm::sot_comm_home();
        let self_host = comm_self_host();
        let topology = sot_protocol::topology::load();
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
            let within = crate::comm::mail::inbox::inbox_lock_wait() + COMM_FORWARD_SLACK;
            let endpoint = match &topology {
                Ok(Some((_, t))) => sot_protocol::topology::relay_endpoint(t, &self_host),
                Ok(None) => Err("no hosts.toml names a hub".to_string()),
                Err(e) => return Err(crate::comm::mail::inbox::refusal::hosts_toml_unreadable(e)),
            };
            let endpoint = endpoint.map_err(|e| crate::comm::mail::inbox::refusal::hub_did_not_answer("the relay endpoint", &e))?;
            crate::comm::mail::forward::forward_comm_file(&endpoint, &self_host, fwd, within, crate::lifecycle::child_signal::process()).map_err(|e| {
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
            crate::comm::mail::inbox::inbox_lock_wait(),
        )
    })
    .await
    .context("comm.file join")
}

/// A guest's forward answers inside the script's read window (the lock wait
/// plus 10 s): the hub's own lock wait plus this.
const COMM_FORWARD_SLACK: std::time::Duration = std::time::Duration::from_secs(5);

/// This host's name — the one `sotd topology` uses — or empty when it has
/// none, which never matches a hub or a record's writer.
pub(crate) fn comm_self_host() -> String {
    sot_log::host::state_dir::host_name().unwrap_or_default()
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
    let not_here = || ("not_here".to_string(), format!("no box knows that handle: {to}"));
    let Some(home) = comm_home else {
        return Err(not_here());
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
    // A missing registry lists nobody; bytes that are not a registry are no
    // answer at all, never "not here".
    let agents = match read_registry_fresh(&home.join("registry.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_here()),
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
        return Err(not_here());
    }
    if !heartbeat_fresh(field("last_seen"), now_secs) {
        return Err(("no_live_session".into(), format!("no live session holds @{to}")));
    }
    let ts = iso8601_utc_from_secs(now_secs);
    crate::comm::mail::inbox::file_frame(&home.join("inbox"), &req.from, to, req.broadcast, &req.text, &ts, wait, own)
        .map_err(|e| ("file_failed".into(), e))
}

/// `comm.file`'s liveness: `last_seen` within `LIVE_SECS` of now.
/// Both writers stamp one fixed-width UTC shape (`now_iso`,
/// `iso8601_utc_now`), so string order is time order; a stamp that is absent
/// or not exactly that shape is no heartbeat, never a fresh one.
fn heartbeat_fresh(last_seen: Option<&str>, now_secs: u64) -> bool {
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

/// How old a `last_seen` may be and still count: the one number, `COMM_LIVE_SECS`
/// in `comm-lib-base.sh`, which `sot_heartbeat_fresh` applies to the same stamp.
const LIVE_SECS: u64 = 600;

#[cfg(test)]
mod comm_file_tests {
    use super::*;
    use crate::comm::registry::registry::read_registry_fresh_with;
    use std::time::Duration;

    const NOW: u64 = 1_790_000_000;

    type Verdict = std::result::Result<(), (String, String)>;

    /// A folder whose record names `local m`, written by machine `m`.
    fn home() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("inbox")).unwrap();
        let reg = json!({"agents": {
            "fresh": {"host": "b", "last_seen": iso8601_utc_from_secs(NOW - 5)},
            "stale": {"host": "b", "last_seen": iso8601_utc_from_secs(NOW - 3600)},
            "hostless": {"host": "", "last_seen": iso8601_utc_from_secs(NOW - 5)},
        }});
        std::fs::write(d.path().join("registry.json"), reg.to_string()).unwrap();
        std::fs::write(d.path().join("inbox-lock-manager"), "local m\nm\n").unwrap();
        d
    }

    fn filer(role: crate::comm::mail::inbox::Role, own: &str) -> Filer {
        Filer { role, own: own.into(), machine_id: Some("m".into()), self_host: "hub-a".into() }
    }

    fn no_forward(_: &CommFileReq) -> std::result::Result<serde_json::Value, String> {
        panic!("a filing at the hub never forwards")
    }

    fn file_as(home: Option<&std::path::Path>, filer: &Filer, req: &CommFileReq) -> Verdict {
        comm_file_verdict(home, filer, no_forward, req, NOW, Duration::from_secs(1))
    }

    fn req(to: &str) -> CommFileReq {
        CommFileReq { from: "s".into(), to: to.into(), text: "hi".into(), broadcast: false, forwarded: false }
    }

    fn file(home: Option<&std::path::Path>, to: &str) -> Verdict {
        file_as(home, &filer(crate::comm::mail::inbox::Role::Hub, "local m"), &req(to))
    }

    fn err(code: &str, text: &str) -> Verdict {
        Err((code.into(), text.into()))
    }

    // T2 — every verdict, and a refusal leaves the inbox untouched.
    #[test]
    fn listed_and_fresh_files_the_line() {
        let d = home();
        assert_eq!(file(Some(d.path()), "fresh"), Ok(()));
        let line = std::fs::read_to_string(d.path().join("inbox/fresh.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v, json!({"from": "s", "to": "fresh", "repo": "daemon", "msg": "hi",
                              "ts": iso8601_utc_from_secs(NOW)}));
    }

    // A broadcast copy is stamped `to:""` and still lands in `to`'s inbox;
    // a request that omits the field is directed.
    #[test]
    fn a_broadcast_copy_is_stamped_to_empty() {
        let d = home();
        let req: CommFileReq =
            serde_json::from_value(json!({"from": "s", "to": "fresh", "text": "all", "broadcast": true})).unwrap();
        assert_eq!(file_as(Some(d.path()), &filer(crate::comm::mail::inbox::Role::Hub, "local m"), &req), Ok(()));
        let line = std::fs::read_to_string(d.path().join("inbox/fresh.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!((v["to"].as_str(), v["repo"].as_str()), (Some(""), Some("daemon")));
        let req: CommFileReq = serde_json::from_value(json!({"from": "s", "to": "fresh", "text": "x"})).unwrap();
        assert!(!req.broadcast);
    }

    #[test]
    fn a_stale_heartbeat_is_no_live_session_and_appends_nothing() {
        let d = home();
        let inbox = d.path().join("inbox/stale.jsonl");
        std::fs::write(&inbox, "{\"msg\":\"before\"}\n").unwrap();
        let (code, error) = file(Some(d.path()), "stale").unwrap_err();
        assert_eq!((code.as_str(), error.as_str()), ("no_live_session", "no live session holds @stale"));
        assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "{\"msg\":\"before\"}\n");
    }

    // An unreadable registry is no verdict on `to`; a missing one lists nobody.
    #[test]
    fn an_empty_registry_is_file_failed_and_a_missing_one_not_here() {
        let d = home();
        std::fs::write(d.path().join("registry.json"), "").unwrap();
        let t0 = std::time::Instant::now();
        let (code, error) = file(Some(d.path()), "fresh").unwrap_err();
        // An empty registry is retried over the 200 ms schedule before the verdict; this is a lower bound, so load only lengthens it.
        // It catches a `file()` that skips the retried read unless that read stalls 200 ms, so it does not pin the path deterministically.
        assert!(t0.elapsed() >= std::time::Duration::from_millis(200), "{:?}", t0.elapsed());
        assert_eq!(code, "file_failed");
        assert!(error.contains("could not be read"), "{error}");
        assert!(!d.path().join("inbox/fresh.jsonl").exists());
        std::fs::remove_file(d.path().join("registry.json")).unwrap();
        assert_eq!(file(Some(d.path()), "fresh").unwrap_err().0, "not_here");
    }

    // The heal runs inside a pause, so which try sees it is fixed; the
    // empty-registry test above pins the verdict arm.

    /// The retried read's pause schedule.
    const SCHEDULE: [std::time::Duration; 3] = [std::time::Duration::from_millis(0), std::time::Duration::from_millis(100), std::time::Duration::from_millis(100)];

    /// Read the registry, running `heal` in pause number `at` (0 = never);
    /// returns the result and the pauses it took, in order.
    fn read_healed(reg: &std::path::Path, at: usize, heal: impl FnOnce()) -> (std::io::Result<Vec<u8>>, Vec<std::time::Duration>) {
        let (mut pauses, mut heal) = (Vec::new(), Some(heal));
        let r = read_registry_fresh_with(reg, |d| {
            pauses.push(d);
            if pauses.len() == at {
                if let Some(h) = heal.take() {
                    h();
                }
            }
        });
        (r, pauses)
    }

    /// The message of an unreadable registry, which is never NotFound.
    fn unreadable(r: std::io::Result<Vec<u8>>) -> String {
        let e = r.unwrap_err();
        assert_ne!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
        e.to_string()
    }

    // A registry that reads empty until a pause is read again; one that
    // stays empty is unreadable.
    #[test]
    fn a_registry_empty_until_a_pause_is_read_and_one_that_stays_empty_is_unreadable() {
        for at in [1, 3, 0] {
            let d = home();
            let (reg, tmp) = (d.path().join("registry.json"), d.path().join("registry.json.tmp"));
            let good = std::fs::read(&reg).unwrap();
            std::fs::copy(&reg, &tmp).unwrap();
            std::fs::write(&reg, "").unwrap();
            let (r, pauses) = read_healed(&reg, at, || std::fs::rename(&tmp, &reg).unwrap());
            if at > 0 {
                assert_eq!(r.unwrap(), good);
                assert_eq!(pauses, SCHEDULE[..at], "the schedule");
            } else {
                assert_eq!(unreadable(r).as_str(), "no good read in 4 tries; the last: zero bytes");
                assert_eq!(pauses, SCHEDULE, "the schedule");
            }
        }
    }

    // A registry whose read fails until a pause (mode 000 here; ESTALE across
    // boxes) is read again; one that fails throughout, or that vanishes while
    // it is retried, is unreadable, never "not here".
    #[cfg(unix)]
    #[test]
    fn a_registry_failing_until_a_pause_is_read_and_one_failing_throughout_or_vanishing_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        for (at, vanish) in [(1, false), (0, false), (1, true)] {
            let d = home();
            let reg = d.path().join("registry.json");
            let good = std::fs::read(&reg).unwrap();
            std::fs::set_permissions(&reg, std::fs::Permissions::from_mode(0o000)).unwrap();
            if std::fs::File::open(&reg).is_ok() {
                eprintln!("skipped: mode 000 does not stop this user's open (root)");
                return;
            }
            let (r, pauses) = read_healed(&reg, at, || {
                if vanish {
                    std::fs::remove_file(&reg).unwrap();
                } else {
                    std::fs::set_permissions(&reg, std::fs::Permissions::from_mode(0o644)).unwrap();
                }
            });
            if at > 0 && !vanish {
                assert_eq!(r.unwrap(), good);
                assert_eq!(pauses, SCHEDULE[..at], "the schedule at {at}, vanish {vanish}");
            } else {
                unreadable(r);
                assert_eq!(pauses, SCHEDULE, "the schedule at {at}, vanish {vanish}");
            }
        }
    }

    // The shape ESTALE takes across boxes: the open succeeds and the read
    // fails. A directory at the registry's path does the same on one box.
    #[test]
    fn a_registry_that_opens_but_will_not_read_until_a_pause_is_read_and_throughout_is_unreadable() {
        for at in [1, 0] {
            let d = home();
            let (reg, tmp) = (d.path().join("registry.json"), d.path().join("registry.json.tmp"));
            let good = std::fs::read(&reg).unwrap();
            std::fs::rename(&reg, &tmp).unwrap();
            std::fs::create_dir(&reg).unwrap();
            let (r, pauses) = read_healed(&reg, at, || {
                std::fs::remove_dir(&reg).unwrap();
                std::fs::rename(&tmp, &reg).unwrap();
            });
            if at > 0 {
                assert_eq!(r.unwrap(), good);
                assert_eq!(pauses, SCHEDULE[..at], "the schedule");
            } else {
                unreadable(r);
                assert_eq!(pauses, SCHEDULE, "the schedule");
            }
        }
    }

    #[test]
    fn unlisted_hostless_and_no_folder_are_not_here() {
        let d = home();
        for to in ["nobody", "hostless"] {
            assert_eq!(file(Some(d.path()), to), err("not_here", &format!("no box knows that handle: {to}")));
        }
        assert_eq!(file(None, "fresh").unwrap_err().0, "not_here");
        assert!(!d.path().join("inbox/nobody.jsonl").exists());
    }

    #[test]
    fn an_empty_or_invalid_to_is_bad_handle() {
        let d = home();
        assert_eq!(file(Some(d.path()), ""), err("bad_handle", "not a handle: \"\""));
        assert_eq!(file(Some(d.path()), "../x").unwrap_err().0, "bad_handle");
    }

    #[test]
    fn an_append_that_cannot_happen_is_file_failed() {
        let d = home();
        std::fs::create_dir(d.path().join("inbox/fresh.jsonl")).unwrap();
        let (code, error) = file(Some(d.path()), "fresh").unwrap_err();
        assert_eq!(code, "file_failed");
        assert!(error.starts_with("the append failed: "), "{error}");
    }

    // B1 — a stale record at the hub is refused with the recovery named,
    // and the inbox is byte-identical.
    #[test]
    fn a_stale_record_at_the_hub_is_file_failed_with_the_recovery() {
        let d = home();
        let inbox = d.path().join("inbox/fresh.jsonl");
        std::fs::write(&inbox, "{\"msg\":\"before\"}\n").unwrap();
        for (record, fragment) in [
            ("nfs4 B:/y\nm\n", "restart this daemon to re-record it after the remount"),
            ("nfs4 B:/y\nm-b\n", "written by machine m-b, a different lock manager from this hub's local m: stop every daemon"),
            ("nfs4 B:/y\n", "written by an unknown machine"),
        ] {
            std::fs::write(d.path().join("inbox-lock-manager"), record).unwrap();
            let (code, error) = file(Some(d.path()), "fresh").unwrap_err();
            assert_eq!(code, "file_failed", "{record:?}");
            assert!(error.contains(fragment), "{record:?}: {error}");
            assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "{\"msg\":\"before\"}\n");
        }
        std::fs::remove_file(d.path().join("inbox-lock-manager")).unwrap();
        let (code, error) = file(Some(d.path()), "fresh").unwrap_err();
        assert!(code == "file_failed" && error.starts_with("no inbox lock record at "), "{error}");
        assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "{\"msg\":\"before\"}\n");
    }

    // B1 — a guest that cannot prove the shared lock forwards, marked
    // forwarded, and answers with the hub's own verdict.
    #[test]
    fn a_guest_with_a_mismatch_forwards_and_returns_the_hubs_answer() {
        let d = home();
        let guest = Filer {
            role: crate::comm::mail::inbox::Role::Guest,
            own: "nfs4 A:/x".into(),
            machine_id: Some("m-b".into()),
            self_host: "guest-b".into(),
        };
        let forward_with = |answer: std::result::Result<serde_json::Value, String>| {
            let sent = std::cell::RefCell::new(None);
            let v = comm_file_verdict(
                Some(d.path()),
                &guest,
                |fwd: &CommFileReq| {
                    *sent.borrow_mut() = Some(fwd.clone());
                    answer
                },
                &req("fresh"),
                NOW,
                Duration::from_secs(1),
            );
            (v, sent.into_inner().expect("forwarded"))
        };
        let (v, sent) = forward_with(Ok(json!({"ok": true})));
        assert_eq!(v, Ok(()));
        assert!(sent.forwarded && sent.to == "fresh" && sent.text == "hi" && !sent.broadcast);
        let (v, _) = forward_with(Ok(json!({"error": "no box knows that handle: fresh", "code": "not_here"})));
        assert_eq!(v, err("not_here", "no box knows that handle: fresh"));
        let (v, _) = forward_with(Err("the hub did not answer at unix:/x: refused".into()));
        assert_eq!(v, err("file_failed", "the hub did not answer at unix:/x: refused"));
        assert!(!d.path().join("inbox/fresh.jsonl").exists(), "a guest's forward appends nothing here");

        // A forwarded frame at a guest is refused, never forwarded again.
        let mut again = req("fresh");
        again.forwarded = true;
        let (code, error) = file_as(Some(d.path()), &guest, &again).unwrap_err();
        assert!(code == "file_failed" && error.contains("not its folder's hub (guest-b)"), "{error}");
    }

    // T4 — the cutoff: one second inside is fresh, one outside and the edge
    // are stale (the shell's `age >= stale`), malformed and absent are no
    // heartbeat.
    #[test]
    fn the_heartbeat_cutoff_is_a_string_compare_on_one_shape() {
        let at = |s: u64| iso8601_utc_from_secs(s);
        assert!(heartbeat_fresh(Some(&at(NOW - 599)), NOW));
        assert!(!heartbeat_fresh(Some(&at(NOW - 600)), NOW));
        assert!(!heartbeat_fresh(Some(&at(NOW - 601)), NOW));
        assert!(!heartbeat_fresh(Some("2026-9-29T01:02:03Z"), NOW));
        assert!(!heartbeat_fresh(Some("9999-99-99 99:99:99Z"), NOW));
        assert!(!heartbeat_fresh(None, NOW));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(NOW).len(), 20);
    }

    // The shell's `sot_heartbeat_fresh` and `heartbeat_fresh` are one rule on
    // one stamp. Unix only: it runs bash, and the hosted macOS leg runs it
    // with BSD `date`.
    #[cfg(unix)]
    #[test]
    fn heartbeat_agrees_with_the_shell() {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/lib/comm-lib.sh");
        let now = unix_now_secs();
        let at = |s: u64| iso8601_utc_from_secs(s);
        // Each stamp with the env its shell run gets.
        let cases: Vec<(String, Option<(&str, &str)>)> = vec![
            (at(now - 590), None),
            (at(now - 610), None),
            (at(now + 3600), None),
            (String::new(), None),
            ("2026-9-29T01:02:03Z".into(), None),
            ("9999-99-99 99:99:99Z".into(), None),
            ("2099-01-01 00:00:00Z".into(), None),
            ("2099-01-01T00:00:00+00:00".into(), None),
            ("2099-01-01T00:00:00z".into(), None),
            (at(now - 100), Some(("SOT_COMM_STALE_SECS", "30"))),
        ];
        for (stamp, extra) in cases {
            let mut cmd = std::process::Command::new("/bin/bash");
            cmd.arg("-c")
                .arg(r#"source "$1"; sot_heartbeat_fresh "$2"; echo $?; echo "$BASH_VERSION""#)
                .arg("bash")
                .arg(&lib)
                .arg(&stamp);
            if let Some((k, v)) = extra {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("run bash");
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let shell = stdout.lines().next() == Some("0");
            let rust = heartbeat_fresh(if stamp.is_empty() { None } else { Some(&stamp) }, now);
            assert_eq!(rust, shell, "bash {}, stamp {stamp:?}: {}", stdout.lines().nth(1).unwrap_or_default(), String::from_utf8_lossy(&out.stderr));
        }
    }

    // The inbox has no writer but the verdict (and the one-time move of an old
    // frontend inbox): every daemon route ends in `comm_file_verdict`, and the
    // last_seen readers and writers, and every open for append, are the listed
    // files in every crate. A new member fails here.
    #[test]
    fn every_inbox_append_is_the_verdicts_or_the_move() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs")
                    && !p.file_name().unwrap().to_string_lossy().ends_with("_tests.rs")
                {
                    out.push(p);
                }
            }
        }
        let rust = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&rust).unwrap() {
            let src = krate.unwrap().path().join("src");
            if src.is_dir() {
                walk(&src, &mut files);
            }
        }
        assert!(files.len() > 200, "the walk found {} files", files.len());
        let word = |text: &str, w: &str| {
            text.match_indices(w).any(|(i, _)| {
                let before = text[..i].chars().next_back();
                let after = text[i + w.len()..].chars().next();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        };
        let (mut appenders, mut last_seen, mut opens) = (Vec::new(), Vec::new(), Vec::new());
        let mut inbox_text = String::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let text = text.split("\n#[cfg(test)]").next().unwrap();
            let rel = f.strip_prefix(&rust).unwrap().to_string_lossy().replace('\\', "/");
            let calls = text.matches("file_frame(").count() - text.matches("pub fn file_frame(").count();
            if calls > 0 {
                appenders.push((rel.clone(), calls));
            }
            if word(text, "last_seen") {
                last_seen.push(rel.clone());
            }
            let n = text.matches(".append(true)").count();
            if n > 0 {
                opens.push((rel.clone(), n));
            }
            if rel == "backend/src/comm/mail/inbox.rs" {
                inbox_text = text.to_string();
            }
        }
        appenders.sort();
        last_seen.sort();
        opens.sort();
        assert_eq!(appenders, vec![("backend/src/comm/mail/filer.rs".to_string(), 1), ("backend/src/comm/mail/hub_link.rs".to_string(), 1)]);
        assert_eq!(
            last_seen,
            [
                "backend/src/comm/mail/filer.rs",
                "backend/src/comm/registry/liveness.rs",
                "backend/src/comm/registry/poll.rs",
                "backend/src/comm/registry/registry.rs",
                "backend/src/server/mod.rs",
            ]
        );
        assert_eq!(
            opens,
            vec![
                ("backend/src/comm/mail/inbox.rs".to_string(), 2),
                ("backend/src/main.rs".to_string(), 1),
                ("backend/src/rows/spawn/detach.rs".to_string(), 1),
            ]
        );
        // `append_line` is defined once and called once, by `file_frame_with`, which `file_frame` calls.
        assert_eq!(inbox_text.matches("append_line(").count(), 2, "append_line has a new caller");
        let from = inbox_text.find("fn file_frame_with(").expect("file_frame_with");
        let body = &inbox_text[from..];
        let end = body[1..].find("\nfn ").or_else(|| body[1..].find("\npub fn ")).map_or(body.len(), |i| i + 1);
        assert_eq!(body[..end].matches("append_line(").count(), 1, "append_line is not called by file_frame_with");
    }
}
