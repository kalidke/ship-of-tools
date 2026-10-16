//! `sotd same-version --endpoint <unix:PATH|pipe:PATH>`: whether the daemon answering on a local endpoint runs this
//! binary's version. The launch scripts' daemon ensure asks it once the endpoint answers
//! (`scripts/sot-local-daemon.ps1`, `scripts/lib/sot-daemon.sh`), so a daemon an update left running on the old binary
//! is replaced instead of kept: its pipe or socket answering says only that some daemon is there.
//!
//! The daemon names its version in its reply to a hello (ADR 0030 §2): `app_version` when it admits the hello,
//! `backend_version` when it refuses it for another protocol. Exit 0: it runs this binary's version. Exit 3: it runs
//! another, printed on stdout. Any other exit (2 here, 1 from a `sotd` that predates this command) means it cannot be
//! told, with one stderr line saying why, and the caller keeps the daemon.

use std::time::Duration;

/// The usage line, printed on a bad argument and for `--help`.
pub(crate) const USAGE: &str = "Usage: sotd same-version --endpoint <unix:PATH|pipe:PATH>";

/// How long the hello reply may take: a daemon whose endpoint accepts answers a hello at once.
const REPLY_BOUND: Duration = Duration::from_secs(5);

const SAME: i32 = 0;
const UNKNOWN: i32 = 2;
const OTHER: i32 = 3;

pub fn run(args: &[String]) -> i32 {
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
            OTHER
        }
        Err(why) => {
            eprintln!("sotd same-version: {why}");
            UNKNOWN
        }
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
            let code = run(&["--endpoint".to_string(), format!("unix:{}", sock.display())]);
            assert_eq!(server.join().expect("the listener thread"), json!("cli"));
            assert_eq!(code, want);
        }
    }
}
