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
