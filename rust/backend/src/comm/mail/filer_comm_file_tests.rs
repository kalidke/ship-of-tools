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
    Filer { role, own: own.into(), machine_id: Some("m".into()), self_host: "hub-a".into(), env: crate::comm::Env::empty() }
}

fn no_forward(_: &CommFileReq) -> std::result::Result<serde_json::Value, String> {
    panic!("a filing at the hub never forwards")
}

fn file_as(home: Option<&std::path::Path>, filer: &Filer, req: &CommFileReq) -> Verdict {
    comm_file_verdict(home, filer, no_forward, req, NOW, Duration::from_secs(1))
}

fn req(to: &str) -> CommFileReq {
    CommFileReq { from: "s".into(), to: to.into(), text: "hi".into(), broadcast: false, forwarded: false, expected_row: None }
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

/// With no `SOT_INBOX_LOCK_WAIT_SECS` a filing waits 2 s for a held lock, the shell's default, and says so.
#[test]
fn the_default_wait_is_the_shells_two_seconds() {
    let d = home();
    let held = std::fs::OpenOptions::new().read(true).create(true).append(true).open(d.path().join("inbox/fresh.lock")).unwrap();
    held.lock().unwrap();
    let filer = filer(crate::comm::mail::inbox::Role::Hub, "local m");
    let wait = crate::comm::mail::inbox::inbox_lock_wait(&crate::comm::Env::empty());
    assert_eq!(
        comm_file_verdict(Some(d.path()), &filer, no_forward, &req("fresh"), NOW, wait),
        err("file_failed", "the inbox lock for @fresh was held for 2s — nothing was appended")
    );
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
        #[allow(clippy::disallowed_methods, reason = "a data copy of a registry file, no program is run from it")]
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
        env: crate::comm::Env::empty(),
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

// The one append behind every route: a handle the folder lists, held by a live session, is filed whole; anything
// else appends nothing (the daemon's `comm.file` and `sotd comm send` both reach the inbox only through it).
#[test]
fn an_append_is_made_only_for_a_listed_live_handle() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    std::fs::create_dir_all(home.join("inbox")).unwrap();
    let live = iso8601_utc_from_secs(NOW - 10);
    let reg = serde_json::json!({"agents": {
        "a": {"host": "h", "last_seen": live},
        "stale": {"host": "h", "last_seen": iso8601_utc_from_secs(NOW - 601)},
        "nohost": {"last_seen": live},
    }});
    std::fs::write(home.join("registry.json"), reg.to_string()).unwrap();
    let w = std::time::Duration::from_secs(1);
    file_here(home, "me", "proj", "a", false, "hi", NOW, w, "local t").unwrap();
    let line: serde_json::Value = serde_json::from_str(std::fs::read_to_string(home.join("inbox/a.jsonl")).unwrap().trim()).unwrap();
    assert_eq!((line["from"].as_str(), line["repo"].as_str(), line["msg"].as_str()), (Some("me"), Some("proj"), Some("hi")));
    assert_eq!(file_here(home, "me", "p", "stale", false, "x", NOW, w, "local t").unwrap_err().0, "no_live_session");
    assert_eq!(file_here(home, "me", "p", "nohost", false, "x", NOW, w, "local t").unwrap_err().0, "not_here");
    assert_eq!(file_here(home, "me", "p", "absent", false, "x", NOW, w, "local t").unwrap_err().0, "not_here");
    std::fs::write(home.join("registry.json"), "not json").unwrap();
    assert_eq!(file_here(home, "me", "p", "a", false, "x", NOW, w, "local t").unwrap_err().0, "file_failed");
    for h in ["stale", "nohost", "absent"] {
        assert!(!home.join(format!("inbox/{h}.jsonl")).exists(), "{h}");
    }
    assert_eq!(std::fs::read_to_string(home.join("inbox/a.jsonl")).unwrap().lines().count(), 1);
}

// P14 — guarded filing: a request naming its expected row is admitted only here, and appends only while the registry,
// read fresh under the registry lock taken after the inbox lock, binds the handle to that row.

/// A folder whose `bound` entry is live and bound to row `ws-1`.
fn bound_home() -> tempfile::TempDir {
    let d = home();
    let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(d.path().join("registry.json")).unwrap()).unwrap();
    doc["agents"]["bound"] = json!({"host": "b", "workspace_id": "ws-1", "last_seen": iso8601_utc_from_secs(NOW - 5)});
    doc["agents"]["other"] = json!({"host": "b", "workspace_id": "ws-1", "last_seen": iso8601_utc_from_secs(NOW - 5)});
    doc["agents"]["bound-stale"] = json!({"host": "b", "workspace_id": "ws-1", "last_seen": iso8601_utc_from_secs(NOW - 3600)});
    doc["agents"]["unbound"] = json!({"host": "b", "workspace_id": "", "last_seen": iso8601_utc_from_secs(NOW - 5)});
    doc["agents"]["malformed"] = json!("not an object");
    std::fs::write(d.path().join("registry.json"), doc.to_string()).unwrap();
    d
}

fn expecting(to: &str, row: &str) -> CommFileReq {
    CommFileReq { expected_row: Some(row.into()), ..req(to) }
}

fn hub_filer() -> Filer {
    filer(crate::comm::mail::inbox::Role::Hub, "local m")
}

/// Alone on the registry's turn: what these assert rests on taking the registry lock within a bound under 10 s.
fn alone() -> std::sync::RwLockWriteGuard<'static, ()> {
    crate::comm::registry::lock::TURN_TESTS.write().unwrap_or_else(|e| e.into_inner())
}

fn guarded(home: &std::path::Path, req: &CommFileReq) -> Verdict {
    let _turn = alone();
    file_as(Some(home), &hub_filer(), req)
}

/// The registry lock is free: a taker gets it at once.
fn lock_is_free(home: &std::path::Path) {
    let env = crate::comm::Env::empty();
    crate::comm::registry::lock::acquire_in(&env, &home.join(".registry.lock"), Duration::from_secs(5)).expect("the lock was left held");
}

#[test]
fn a_matching_row_files_and_an_absent_expectation_files_as_before() {
    let d = bound_home();
    assert_eq!(guarded(d.path(), &expecting("bound", "ws-1")), Ok(()));
    assert_eq!(guarded(d.path(), &req("bound")), Ok(()));
    assert_eq!(guarded(d.path(), &req("unbound")), Ok(()));
    assert_eq!(std::fs::read_to_string(d.path().join("inbox/bound.jsonl")).unwrap().lines().count(), 2);
    lock_is_free(d.path());
}

#[test]
fn a_moved_binding_is_not_here_naming_the_move_and_appends_nothing() {
    let d = bound_home();
    for (to, row, now) in [("bound", "ws-2", "ws-1"), ("unbound", "ws-2", "no row")] {
        let (code, error) = guarded(d.path(), &expecting(to, row)).unwrap_err();
        assert!(code == "not_here" && error.contains("no longer bound") && error.contains(now), "{to}: {code} {error}");
        assert!(!d.path().join(format!("inbox/{to}.jsonl")).exists(), "{to}");
    }
    // An entry with no row never matches a named row, and a named row is never empty.
    assert_eq!(guarded(d.path(), &expecting("unbound", "")).unwrap_err().0, "file_failed");
    // Presence and liveness keep their own refusals; an unlisted handle is not bound here.
    for to in ["nobody", "malformed", "hostless"] {
        let (code, error) = guarded(d.path(), &expecting(to, "ws-1")).unwrap_err();
        assert!(code == "not_here" && error.contains("not bound here"), "{to}: {error}");
    }
    assert_eq!(guarded(d.path(), &expecting("bound-stale", "ws-1")).unwrap_err().0, "no_live_session");
    assert!(!d.path().join("inbox/nobody.lock").exists(), "a refusal left an inbox lock file");
    lock_is_free(d.path());
}

#[test]
fn a_registry_that_cannot_be_read_is_file_failed_when_guarded() {
    let d = bound_home();
    std::fs::write(d.path().join("registry.json"), "{bad").unwrap();
    assert_eq!(guarded(d.path(), &expecting("bound", "ws-1")).unwrap_err().0, "file_failed");
    assert!(!d.path().join("inbox/bound.jsonl").exists());
    lock_is_free(d.path());
}

/// The plan's proof: the filing waits on the registry lock, the binding moves through its normal writer while the lock is
/// held, and the released filing sees the move and appends nothing.
#[test]
fn a_binding_moved_while_the_filing_waits_is_seen_and_nothing_is_appended() {
    let d = bound_home();
    let _turn = alone();
    let env = crate::comm::Env::empty();
    let held = crate::comm::registry::lock::acquire_in(&env, &d.path().join(".registry.lock"), Duration::from_secs(5)).unwrap();
    let path = d.path().to_path_buf();
    let filing = std::thread::spawn(move || {
        comm_file_verdict(Some(&path), &hub_filer(), no_forward, &expecting("bound", "ws-1"), NOW, Duration::from_secs(8))
    });
    std::thread::sleep(Duration::from_millis(300));
    let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(d.path().join("registry.json")).unwrap()).unwrap();
    doc["agents"]["bound"]["workspace_id"] = json!("ws-2");
    crate::comm::registry::registry::write_registry(&d.path().join("registry.json"), &d.path().join("registry.json.tmp"), &doc).unwrap();
    drop(held);
    let (code, error) = filing.join().unwrap().unwrap_err();
    assert!(code == "not_here" && error.contains("no longer bound") && error.contains("ws-2"), "{code} {error}");
    assert!(!d.path().join("inbox/bound.jsonl").exists());
    lock_is_free(d.path());
}

/// A registry lock a foreign holder keeps is an explicit refusal at the bound, nothing appended, and the inbox lock is let go.
#[test]
fn a_registry_lock_held_elsewhere_is_an_explicit_refusal_and_appends_nothing() {
    let d = bound_home();
    let _turn = alone();
    std::fs::write(d.path().join(".registry.lock"), "someone:-:-:-:1:-\n").unwrap();
    let (code, error) = comm_file_verdict(Some(d.path()), &hub_filer(), no_forward, &expecting("bound", "ws-1"), NOW, Duration::from_secs(1)).unwrap_err();
    assert!(code == "file_failed" && error.contains("nothing was filed"), "{code} {error}");
    assert!(!d.path().join("inbox/bound.jsonl").exists());
    assert!(d.path().join(".registry.lock").exists(), "a foreign holder's lock is never forced");
    // The inbox lock is free: an ordinary filing to the same handle takes it.
    assert_eq!(comm_file_verdict(Some(d.path()), &hub_filer(), no_forward, &req("bound"), NOW, Duration::from_secs(1)), Ok(()));
}

/// A stuck inbox is that handle's problem: the filing waits for it without the registry lock, so a registry writer is
/// never held up, and the one wait ends the filing.
#[test]
fn a_stuck_inbox_never_holds_the_registry_lock() {
    let d = bound_home();
    let _turn = alone();
    let inbox_lock = std::fs::OpenOptions::new().read(true).create(true).append(true).open(d.path().join("inbox/bound.lock")).unwrap();
    inbox_lock.lock().unwrap();
    let path = d.path().to_path_buf();
    let filing = std::thread::spawn(move || {
        comm_file_verdict(Some(&path), &hub_filer(), no_forward, &expecting("bound", "ws-1"), NOW, Duration::from_secs(1))
    });
    std::thread::sleep(Duration::from_millis(400));
    assert!(!d.path().join(".registry.lock").exists(), "the registry lock is not taken while the inbox is awaited");
    let (code, _) = filing.join().unwrap().unwrap_err();
    assert_eq!(code, "file_failed");
    lock_is_free(d.path());
}

/// Both waits are one: an inbox released late leaves the registry lock only what is left of the one bound.
#[test]
fn the_inbox_and_registry_waits_share_one_deadline() {
    let d = bound_home();
    let _turn = alone();
    std::fs::write(d.path().join(".registry.lock"), "someone:-:-:-:1:-\n").unwrap();
    let inbox_lock = std::fs::OpenOptions::new().read(true).create(true).append(true).open(d.path().join("inbox/bound.lock")).unwrap();
    inbox_lock.lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        drop(inbox_lock);
    });
    let t0 = std::time::Instant::now();
    let (code, _) = comm_file_verdict(Some(d.path()), &hub_filer(), no_forward, &expecting("bound", "ws-1"), NOW, Duration::from_secs(1)).unwrap_err();
    release.join().unwrap();
    assert_eq!(code, "file_failed");
    assert!(t0.elapsed() < Duration::from_millis(1400), "{:?}: two waits, not one", t0.elapsed());
}

/// The registry lock is held through the append itself, taken after the inbox lock and let go after it.
#[test]
fn the_registry_lock_is_held_through_the_append() {
    let d = bound_home();
    let _turn = alone();
    let env = crate::comm::Env::empty();
    let held_during = std::cell::Cell::new(false);
    let verdict = file_guarded(&env, d.path(), "s", "bound", false, "hi", NOW, Duration::from_secs(1), "local m", "ws-1", |f| {
        // Taking the lock from here fails: the filing holds it (the turn is process-wide).
        let again = crate::comm::registry::lock::acquire_in(&env, &d.path().join(".registry.lock"), Duration::ZERO);
        held_during.set(again.is_err() && d.path().join(".registry.lock").exists());
        f.sync_data()
    });
    assert_eq!(verdict, Ok(()));
    assert!(held_during.get(), "the registry lock was not held while the line was written");
    lock_is_free(d.path());
}

/// A guest cannot append for the row, and the request is refused, never forwarded; an ordinary one still forwards.
#[test]
fn a_guest_refuses_a_request_naming_its_row_and_forwards_nothing() {
    let d = bound_home();
    let guest = Filer {
        role: crate::comm::mail::inbox::Role::Guest,
        own: "nfs4 A:/x".into(),
        machine_id: Some("m-b".into()),
        self_host: "guest-b".into(),
        env: crate::comm::Env::empty(),
    };
    let (code, error) = file_as(Some(d.path()), &guest, &expecting("bound", "ws-1")).unwrap_err();
    assert!(code == "not_here" && error.contains("never forwarded"), "{code} {error}");
    assert!(!d.path().join("inbox/bound.jsonl").exists());
}

/// A hub never offers a request naming its row to the linked daemons; an ordinary one it does.
#[tokio::test]
async fn a_request_naming_its_row_is_never_offered() {
    let t = crate::comm::test_home::TestHome::exclusive();
    t.own_record();
    let links = crate::comm::mail::links::Links::default();
    let mut link = Some(links.attach("far".to_string()));
    let offered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = offered.clone();
    tokio::spawn(async move {
        loop {
            let offer = crate::comm::mail::links::next_offer(&mut link).await;
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let end = link.as_mut().unwrap();
            let frame = end.request(offer).unwrap();
            end.answer(Frame::res(frame.id, op::COMM_FILE, json!({"error": "no box knows that handle: ghost", "code": "not_here"})));
        }
    });
    let ordinary = file_request(t.env.clone(), &req("ghost"), &links).await.unwrap();
    assert_eq!(ordinary.unwrap_err().0, "not_here");
    assert_eq!(offered.load(std::sync::atomic::Ordering::SeqCst), 1, "an ordinary request is offered");
    let (code, error) = file_request(t.env.clone(), &expecting("ghost", "ws-1"), &links).await.unwrap().unwrap_err();
    assert!(code == "not_here" && error.contains("not bound here"), "{code} {error}");
    assert_eq!(offered.load(std::sync::atomic::Ordering::SeqCst), 1, "a request naming its row was offered");
}

/// The wire: an ordinary request neither carries nor needs the field, and a named one round-trips.
#[test]
fn the_expected_row_is_an_optional_wire_field() {
    let plain: CommFileReq = serde_json::from_value(json!({"from": "s", "to": "x", "text": "t"})).unwrap();
    assert_eq!(plain.expected_row, None);
    assert!(serde_json::to_value(&plain).unwrap().get("expected_row").is_none());
    let named: CommFileReq = serde_json::from_value(json!({"from": "s", "to": "x", "text": "t", "expected_row": "ws-1"})).unwrap();
    assert_eq!(named.expected_row.as_deref(), Some("ws-1"));
}
