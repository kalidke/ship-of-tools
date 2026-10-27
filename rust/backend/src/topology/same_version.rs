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
//! told, with one stderr line saying why, and the caller keeps the daemon. With `--json` the answer is one JSON
//! [`Answer`] on stdout instead, with the same exit codes: the installer's one reading of it.

use std::time::Duration;

/// The usage line, printed on a bad argument and for `--help`.
pub(crate) const USAGE: &str = "Usage: sotd same-version --endpoint <unix:PATH|pipe:PATH> [--json]";

/// How long the hello reply may take: a daemon whose endpoint accepts answers a hello at once.
const REPLY_BOUND: Duration = Duration::from_secs(5);

const SAME: i32 = 0;
const UNKNOWN: i32 = 2;
const OTHER: i32 = 3;
const OTHER_LEASED: i32 = 4;

pub fn run(args: &[String]) -> i32 {
    run_in(args, sot_platform::host::state_dir::sot_state_dir().as_deref())
}

/// `run` against the daemon whose state root is `state_root`, where its lease record lives.
fn run_in(args: &[String], state_root: Option<&std::path::Path>) -> i32 {
    let (endpoint, json) = match args {
        [flag, ep] if flag == "--endpoint" && (ep.starts_with("unix:") || ep.starts_with("pipe:")) => (ep, false),
        [flag, ep, j] if flag == "--endpoint" && j == "--json" && (ep.starts_with("unix:") || ep.starts_with("pipe:")) => (ep, true),
        _ => {
            eprintln!("{USAGE}");
            return UNKNOWN;
        }
    };
    let answer = match sot_platform::host::state_dir::host_name() {
        Err(e) => Answer::Untold(e),
        Ok(me) => match crate::topology::dial::hello_reply(endpoint, &me, REPLY_BOUND)
            .and_then(|r| verdict(&r, &sot_protocol::app_version()).map_err(|why| format!("{endpoint}: {why}")))
        {
            Ok(None) => Answer::Same,
            Ok(Some(theirs)) if lease_held(state_root) => Answer::OtherHeld(theirs),
            Ok(Some(theirs)) => Answer::Other(theirs),
            Err(why) => Answer::Untold(why),
        },
    };
    if json {
        let (code, out) = json_answer(&answer);
        print!("{out}");
        return code;
    }
    let printed = print(&answer);
    print!("{}", printed.out);
    eprint!("{}", printed.err);
    printed.code
}

/// What `sotd same-version` answers, as a value: [`print`] writes it as text (an exit code and two streams, the
/// launchers' reading), and [`json_answer`] as one JSON value (the installer's, which deserializes this same type).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Answer {
    /// It runs this binary's version.
    Same,
    /// It runs another, named, and no window holds its lease.
    Other(String),
    /// It runs another, named, and a window holds its lease (or the record cannot be read).
    OtherHeld(String),
    /// It cannot be told, and why.
    Untold(String),
}

/// An answer as the command gives it: its exit code, stdout and stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Printed {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// The exit code that goes with an answer.
pub(crate) fn code(answer: &Answer) -> i32 {
    match answer {
        Answer::Same => SAME,
        Answer::Other(_) => OTHER,
        Answer::OtherHeld(_) => OTHER_LEASED,
        Answer::Untold(_) => UNKNOWN,
    }
}

pub(crate) fn print(answer: &Answer) -> Printed {
    let (out, err) = match answer {
        Answer::Same => (String::new(), String::new()),
        Answer::Other(v) | Answer::OtherHeld(v) => (format!("{v}\n"), String::new()),
        Answer::Untold(why) => (String::new(), format!("sotd same-version: {why}\n")),
    };
    Printed { code: code(answer), out, err }
}

/// An answer as `--json` gives it: its exit code and its one line of stdout.
pub(crate) fn json_answer(answer: &Answer) -> (i32, String) {
    let line = serde_json::to_string(answer).unwrap_or_else(|e| format!("{{\"untold\":\"the answer could not be written: {e}\"}}"));
    (code(answer), format!("{line}\n"))
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
    let field = |k: &str| reply.get(k).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
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
#[path = "same_version_tests.rs"]
mod tests;
