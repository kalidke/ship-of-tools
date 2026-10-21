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

/// With no `SOT_INBOX_LOCK_WAIT_SECS` a filing waits 2 s for a held lock, the shell's default, and says so.
#[test]
fn the_default_wait_is_the_shells_two_seconds() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _wait = crate::paths::EnvGuard::capture(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV);
    std::env::remove_var(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV);
    let d = home();
    let held = std::fs::OpenOptions::new().read(true).create(true).append(true).open(d.path().join("inbox/fresh.lock")).unwrap();
    held.lock().unwrap();
    let filer = filer(crate::comm::mail::inbox::Role::Hub, "local m");
    let wait = crate::comm::mail::inbox::inbox_lock_wait();
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
