//! `sotd same-version --endpoint <unix:PATH|pipe:PATH>`: whether the daemon answering on a local endpoint runs this
//! binary's version. The launch scripts' daemon ensure asks it once the endpoint answers
//! (`scripts/sot-local-daemon.ps1`, `scripts/lib/sot-daemon.sh`), so a daemon an update left running on the old binary
//! is replaced instead of kept: its pipe or socket answering says only that some daemon is there.
//!
//! The daemon names its version in its reply to a hello (ADR 0030 §2): `app_version` when it admits the hello,
//! `backend_version` when it refuses it for another protocol. Exit 0: it runs this binary's version. Exit 3: it runs
//! another, printed on stdout, and no window holds its lease. Exit 4: it runs another, printed on stdout, and a window
//! holds a lease (the daemon's own record, `held.json`, names a holder) or the record cannot be read: the caller keeps
//! it until no window does, and says so. Any other exit (2 here, 1 from a `sotd` that predates this command) means it cannot be
//! told, with one stderr line saying why, and the caller keeps the daemon.

use std::time::Duration;

/// The usage line, printed on a bad argument and for `--help`.
pub(crate) const USAGE: &str = "Usage: sotd same-version --endpoint <unix:PATH|pipe:PATH>";

/// How long the hello reply may take: a daemon whose endpoint accepts answers a hello at once.
const REPLY_BOUND: Duration = Duration::from_secs(5);

const SAME: i32 = 0;
const UNKNOWN: i32 = 2;
const OTHER: i32 = 3;
const OTHER_LEASED: i32 = 4;

pub fn run(args: &[String]) -> i32 {
    run_in(args, sot_log::host::state_dir::sot_state_dir().as_deref())
}

/// `run` against the daemon whose state root is `state_root`, where its lease record lives.
fn run_in(args: &[String], state_root: Option<&std::path::Path>) -> i32 {
    let endpoint = match args {
        [flag, ep] if flag == "--endpoint" && (ep.starts_with("unix:") || ep.starts_with("pipe:")) => ep,
        _ => {
            eprintln!("{USAGE}");
            return UNKNOWN;
        }
    };
    let me = match sot_log::host::state_dir::host_name() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("sotd same-version: {e}");
            return UNKNOWN;
        }
    };
    let reply = crate::topology::dial::hello_reply(endpoint, &me, REPLY_BOUND);
    match reply.and_then(|r| verdict(&r, &sot_protocol::app_version()).map_err(|why| format!("{endpoint}: {why}"))) {
        Ok(None) => SAME,
        Ok(Some(theirs)) => {
            println!("{theirs}");
            if lease_held(state_root) {
                OTHER_LEASED
            } else {
                OTHER
            }
        }
        Err(why) => {
            eprintln!("sotd same-version: {why}");
            UNKNOWN
        }
    }
}

/// Whether a window holds a lease on the daemon whose state root is `state_root`, from its own record: a holder in
/// `held.json`. No record means none (the daemon deletes an empty one). A record that cannot be read, or a state root
/// that cannot be named, cannot tell, and the answer is yes: replacing a daemon under a window is the harm to avoid.
fn lease_held(state_root: Option<&std::path::Path>) -> bool {
    let Some(root) = state_root else { return true };
    match crate::lifecycle::lease::read_record(&root.join(sot_protocol::ops::lease::HELD_RECORD_FILE)) {
        Ok(None) => false,
        Ok(Some(record)) => !record.holders.is_empty(),
        Err(_) => true,
    }
}

/// What the daemon's hello reply says against `ours`: `None` when it runs `ours`, its version when it runs another,
/// and why not when it does not say. A refusal for another protocol is another version whatever it names.
fn verdict(reply: &serde_json::Value, ours: &str) -> Result<Option<String>, String> {
    let field = |k: &str| reply.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    if field("code") == Some("protocol_mismatch") {
        return Ok(Some(field("backend_version").unwrap_or("a version it does not name").to_string()));
    }
    if let Some(error) = reply.get("error") {
        return Err(format!("hello refused: {}", error.as_str().map_or_else(|| error.to_string(), str::to_string)));
    }
    match field("app_version") {
        Some(theirs) if theirs == ours => Ok(None),
        Some(theirs) => Ok(Some(theirs.to_string())),
        None => Err("the hello reply names no version".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_verdict_reads_the_version_from_an_admitted_hello_or_a_protocol_refusal() {
        let ours = "0.6.6-rc9.13";
        let cases = [
            (json!({ "session_id": "s", "app_version": ours }), Ok(None)),
            (json!({ "session_id": "s", "app_version": "0.6.6-rc9.12" }), Ok(Some("0.6.6-rc9.12".to_string()))),
            // The reply an rc1 daemon gave an rc9.13 window on a Windows field box, 2026-10-16.
            (
                json!({ "error": "protocol mismatch", "code": "protocol_mismatch", "backend_version": "0.6.6-rc1" }),
                Ok(Some("0.6.6-rc1".to_string())),
            ),
            (
                json!({ "error": "protocol mismatch", "code": "protocol_mismatch" }),
                Ok(Some("a version it does not name".to_string())),
            ),
            (
                json!({ "error": "a hello must name its host", "code": "identity_missing" }),
                Err("hello refused: a hello must name its host".to_string()),
            ),
            (json!({ "session_id": "s" }), Err("the hello reply names no version".to_string())),
        ];
        for (reply, want) in cases {
            assert_eq!(verdict(&reply, ours), want, "{reply}");
        }
    }

    #[test]
    fn a_lease_is_held_when_the_record_names_a_holder_or_cannot_be_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = dir.path().join("held.json");
        assert!(!lease_held(Some(dir.path())), "no record: no holder");
        let held = |holders: &str| {
            format!(r#"{{"v":1,"boot":"b","holders":{holders},"handover_until_ms":null,"closing":false,"not_ended":0,"forget":[]}}"#)
        };
        std::fs::write(&record, held(r#"[{"boot":"b","pid":1,"created":2}]"#)).unwrap();
        assert!(lease_held(Some(dir.path())), "a holder");
        std::fs::write(&record, held("[]")).unwrap();
        assert!(!lease_held(Some(dir.path())), "a record with no holder, as a pending handover writes");
        std::fs::write(&record, "{not json").unwrap();
        assert!(lease_held(Some(dir.path())), "an unreadable record cannot tell, so the daemon is kept");
        assert!(lease_held(None), "no state root cannot tell either");
    }

    /// The whole command against a listener that answers its hello as a daemon of another protocol does: exit 3 and
    /// that daemon's version, read off the wire. One that admits the hello with this binary's version: exit 0.
    #[cfg(unix)]
    #[test]
    fn same_version_reads_a_live_reply() {
        use std::io::{BufRead, Write};
        let dir = tempfile::tempdir().expect("tempdir");
        for (payload, want) in [
            (
                json!({ "error": "protocol mismatch", "code": "protocol_mismatch", "backend_version": "0.6.6-rc1" }),
                OTHER,
            ),
            (json!({ "session_id": "s", "revision": 0, "app_version": sot_protocol::app_version() }), SAME),
        ] {
            let sock = dir.path().join(format!("d{want}.sock"));
            let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
            let server = std::thread::spawn(move || {
                let (conn, _) = listener.accept().expect("accept");
                let mut line = String::new();
                std::io::BufReader::new(&conn).read_line(&mut line).expect("read the hello");
                let hello: serde_json::Value = serde_json::from_str(&line).expect("the hello parses");
                let frame = json!({ "v": 3, "id": hello["id"], "kind": "res", "op": "hello", "payload": payload });
                (&conn).write_all(format!("{frame}\n").as_bytes()).expect("reply");
                hello["payload"]["role"].clone()
            });
            let state = tempfile::tempdir().expect("an empty state root: no lease is held");
            let code = run_in(&["--endpoint".to_string(), format!("unix:{}", sock.display())], Some(state.path()));
            assert_eq!(server.join().expect("the listener thread"), json!("cli"));
            assert_eq!(code, want);
        }
    }

    /// The same daemon of another protocol, with a window's lease recorded in its state root: exit 4, not 3, so the
    /// caller keeps it.
    #[cfg(unix)]
    #[test]
    fn same_version_of_another_version_under_a_lease_exits_4() {
        use std::io::{BufRead, Write};
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("d.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let server = std::thread::spawn(move || {
            let (conn, _) = listener.accept().expect("accept");
            let mut line = String::new();
            std::io::BufReader::new(&conn).read_line(&mut line).expect("read the hello");
            let hello: serde_json::Value = serde_json::from_str(&line).expect("the hello parses");
            let payload = json!({ "error": "protocol mismatch", "code": "protocol_mismatch", "backend_version": "0.6.6-rc1" });
            let frame = json!({ "v": 3, "id": hello["id"], "kind": "res", "op": "hello", "payload": payload });
            (&conn).write_all(format!("{frame}\n").as_bytes()).expect("reply");
        });
        let state = tempfile::tempdir().expect("state root");
        std::fs::write(
            state.path().join("held.json"),
            r#"{"v":1,"boot":"b","holders":[{"boot":"b","pid":1,"created":2}],"handover_until_ms":null,"closing":false,"not_ended":0,"forget":[]}"#,
        )
        .unwrap();
        let code = run_in(&["--endpoint".to_string(), format!("unix:{}", sock.display())], Some(state.path()));
        server.join().expect("the listener thread");
        assert_eq!(code, OTHER_LEASED);
    }
}
