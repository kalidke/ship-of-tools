//! The Codex wake's one command: the row's own Codex queue, run through the row's launcher. Nothing here can type:
//! this module names no terminal client.
//!
//! [`queue`] runs `ccx --queue <conversation id> <line>` in the row's folder. The launcher (`agents/codex/bin/ccx`,
//! the one the row's leg runs) resolves the binary and its home as it does for the leg, asks the binary's help
//! whether its queue command takes `--thread` and `--message`, and only then runs
//! `codex queue --thread <id> --message <line>`. The child leads a kill domain of the daemon's (containment's
//! `Signal::output`): at `within` its whole tree is asked to end, and once the daemon's child signal has fired no
//! child starts and one in flight is ended.
//!
//! Three outcomes ([`Queued`]). Accepted is Codex 0.171.0's own acknowledgment, `Queued message <id> for thread
//! <id>.`, on a line of stdout with status 0, naming the conversation asked for; it says the request is stored, not
//! that it has run (measured: an open session took one 3 to 10 s later, and after a running turn). Refused is a
//! refusal that happened before Codex's queue command ran, so nothing was stored: no launcher, an installed launcher
//! older than the mode (it would start a session, so it is never run), or the launcher's own refusal, status 69 with a
//! stderr line that starts `ccx: no queue:` (no binary, or one whose help names no such command). Everything else is
//! unclear: a start that failed, the bound, a signal, any other status (Codex's own refusals have not been measured,
//! so none is read as definite), and a status 0 without that acknowledgment. Both streams are read whole, unbounded
//! but for the wait.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The launcher and Codex's queue command together. The command returned in under a second in every trial; the
/// launcher asks the binary's help first, and both start a runtime.
pub(super) const QUEUE_WITHIN: Duration = Duration::from_secs(10);
/// The launcher's status for a refusal of its own, and the start of its line.
const NO_QUEUE_STATUS: i32 = 69;
const NO_QUEUE_LINE: &str = "ccx: no queue:";
/// The longest piece of a child's output kept in an outcome.
const SAID_MAX: usize = 200;

/// What one run of the queue command means (the module's text has the rule).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Queued {
    Accepted,
    Refused(String),
    Unclear(String),
}

/// Runs the row's queue command once for the conversation `thread`, in `cwd` under `env` (a whole environment, not
/// additions), and waits at most `within`. Blocking.
pub(super) fn queue(signal: &'static sot_containment::Signal, ccx: &Path, env: &[(OsString, OsString)], cwd: &Path, thread: &str, line: &str, within: Duration) -> Queued {
    // The mode is known by the launcher's text. Limits: a `ccx` of a person's own that wraps the installed one lacks the
    // text and is refused at every interval, with only the daemon's warning to say so; and any file at that name whose
    // text holds `--queue)` anywhere, a comment included, is run with the three arguments.
    match std::fs::read_to_string(ccx) {
        Ok(text) if text.contains("--queue)") => {}
        Ok(_) => return Queued::Refused(format!("{} is older than its --queue mode", ccx.display())),
        Err(e) => return Queued::Refused(format!("{}: {e}", ccx.display())),
    }
    let mut cmd = std::process::Command::new(ccx);
    cmd.args(["--queue", thread, line]).current_dir(cwd).env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    let out = match signal.output(&mut cmd, within) {
        Ok(out) => out,
        Err(e) => return Queued::Unclear(e.to_string()),
    };
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() && stdout.lines().any(|l| acknowledges(l, thread)) {
        return Queued::Accepted;
    }
    let clip = |s: &str| s.chars().take(SAID_MAX).collect::<String>();
    match stderr.lines().find(|l| l.starts_with(NO_QUEUE_LINE)) {
        Some(said) if out.status.code() == Some(NO_QUEUE_STATUS) => Queued::Refused(clip(said)),
        _ => Queued::Unclear(format!("{}: {}", out.status, clip(stderr.lines().next().unwrap_or("nothing on stderr")))),
    }
}

/// Whether `line` is Codex's acknowledgment of a request stored for `thread`.
fn acknowledges(line: &str, thread: &str) -> bool {
    let named = line.strip_prefix("Queued message ").and_then(|l| l.strip_suffix('.')).and_then(|l| l.split_once(" for thread "));
    named.is_some_and(|(item, to)| is_id(item) && to.eq_ignore_ascii_case(thread))
}

/// A conversation id as the binding hook writes it: a UUID's 36 characters.
pub(super) fn is_id(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() })
}

#[cfg(all(test, target_os = "linux"))]
#[path = "codex_tests.rs"]
mod tests;
