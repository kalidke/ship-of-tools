use super::*;
use crate::comm::registry::registry::read_registry_fresh_with;
use std::time::Duration;

#[cfg(unix)]
const BOUND: Duration = Duration::from_secs(30);

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
        let ran = sot_log::test_wait::run_within(&mut cmd, BOUND);
        let shell = ran.stdout.lines().next() == Some("0");
        let rust = heartbeat_fresh(if stamp.is_empty() { None } else { Some(&stamp) }, now);
        assert_eq!(rust, shell, "bash {}, stamp {stamp:?}: {ran}", ran.stdout.lines().nth(1).unwrap_or_default());
    }
}

// Every daemon route ends in `comm_file_verdict`; the one appender chain is `file_frame` -> `file_frame_with` ->
// `append_line`; the `last_seen` users and every open for append are the listed files in every crate; a new
// member fails here.
#[test]
fn every_inbox_append_is_the_verdicts_or_the_move() {
    use sot_log::test_scan::{enclosing, is_ident, production_sources};
    let files = production_sources();
    let word = |text: &str, w: &str| {
        text.match_indices(w).any(|(i, _)| {
            !is_ident(text[..i].chars().next_back()) && !is_ident(text[i + w.len()..].chars().next())
        })
    };
    // The non-definition calls of `name(` in `text`: their positions.
    let calls = |text: &str, name: &str| -> Vec<usize> {
        text.match_indices(name)
            .filter(|(i, _)| !is_ident(text[..*i].chars().next_back()) && !text[..*i].trim_end().ends_with("fn"))
            .map(|(i, _)| i)
            .collect()
    };
    let (mut appenders, mut last_seen, mut opens) = (Vec::new(), Vec::new(), Vec::new());
    let mut inbox_text = String::new();
    for (path, text) in &files {
        let n = calls(text, "file_frame(").len();
        if n > 0 {
            appenders.push((path.clone(), n));
        }
        if word(text, "last_seen") {
            last_seen.push(path.clone());
        }
        let n = text.matches(".append(true)").count();
        if n > 0 {
            opens.push((path.clone(), n));
        }
        if path == "rust/backend/src/comm/mail/inbox.rs" {
            inbox_text = text.clone();
        }
    }
    appenders.sort();
    last_seen.sort();
    opens.sort();
    assert_eq!(
        appenders,
        vec![("rust/backend/src/comm/mail/filer.rs".to_string(), 1), ("rust/backend/src/comm/mail/hub_link.rs".to_string(), 1)]
    );
    // The heartbeat's readers and the daemon's stamp, and the session's own stamp through work-state's one write.
    assert_eq!(
        last_seen,
        ["rust/backend/src/comm/mail/filer.rs", "rust/backend/src/comm/registry/registry.rs", "rust/backend/src/rows/work_state/mod.rs"]
    );
    assert_eq!(
        opens,
        vec![
            ("rust/backend/src/comm/mail/inbox.rs".to_string(), 2),
            ("rust/backend/src/daemon_log/mod.rs".to_string(), 1),
            ("rust/backend/src/rows/spawn/detach.rs".to_string(), 1),
        ]
    );
    // The chain in inbox.rs: one caller of each link, in the previous one.
    let with = calls(&inbox_text, "file_frame_with(");
    assert_eq!(with.len(), 1, "file_frame_with has a new caller");
    assert_eq!(enclosing(&inbox_text, with[0]), "file_frame");
    let append = calls(&inbox_text, "append_line(");
    assert_eq!(append.len(), 1, "append_line has a new caller");
    assert_eq!(enclosing(&inbox_text, append[0]), "file_frame_with");
}
