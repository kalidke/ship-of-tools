//! `sotd work-state <verb> --handle <h> [--origin user|machine] [--text] [--lock-wait <secs>]`: one work-state write
//! for a session, the way `comm-status.sh` reaches [`super::write_event`]. The text, when `--text` is given, is read from
//! standard input, never from an argument: free text may start with `/`, which Git Bash rewrites in a native program's
//! argv. Exit 0 when written or when there is nothing to write (no entry, a tick with no turn); 1 when the registry
//! could not be locked, read or written, with one FAILED line on stderr; 2 for a refused stamp (a question or a wait
//! with no text) or a bad argument. `comm-status.sh` keeps its own identity, gate and markers.

use std::io::Read;
use std::time::Duration;

use super::{write_event, Declaration, Event, Origin, StampError};
use crate::comm::registry::registry::EditError;

pub(crate) const USAGE: &str = "usage: sotd work-state <prompt|stop|dialog|resumed|tick|working|idle|blocked|waiting|done> \
--handle <h> [--origin user|machine] [--text] [--lock-wait <secs>]";

/// The registry lock's wait when `--lock-wait` is not given: `comm-lib.sh`'s `SOT_LOCK_WAIT_SECS`.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// The event `verb` names, with `text` (`None`: the verb was given no text).
fn event_for(verb: &str, origin: Origin, text: Option<String>) -> Option<Event> {
    let declare = |d| Some(Event::Declare(d, text.clone()));
    match verb {
        "prompt" => Some(Event::Prompt(origin)),
        "stop" => Some(Event::Stop),
        "dialog" => Some(Event::Dialog(text.clone().unwrap_or_default())),
        "resumed" => Some(Event::Resumed),
        "tick" => Some(Event::Tick),
        "working" => declare(Declaration::Working),
        "idle" => declare(Declaration::Idle),
        "blocked" => declare(Declaration::Blocked),
        "waiting" => declare(Declaration::Waiting),
        "done" => declare(Declaration::Done),
        _ => None,
    }
}

/// The parsed command line: the event, the handle and the lock's wait; `Err` is the line to print.
fn parse(args: &[String], read_text: impl FnOnce() -> std::io::Result<String>) -> Result<(Event, String, Duration), String> {
    let mut it = args.iter();
    let verb = it.next().ok_or("no verb")?.clone();
    let (mut handle, mut origin, mut want_text, mut lock_wait) = (None, Origin::Machine, false, LOCK_WAIT);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--handle" => handle = Some(it.next().ok_or("--handle needs a value")?.clone()),
            "--origin" => {
                origin = match it.next().map(String::as_str) {
                    Some("user") => Origin::User,
                    Some("machine") => Origin::Machine,
                    _ => return Err("--origin is user or machine".into()),
                }
            }
            "--text" => want_text = true,
            "--lock-wait" => {
                let secs: u64 = it.next().and_then(|v| v.parse().ok()).ok_or("--lock-wait needs whole seconds")?;
                lock_wait = Duration::from_secs(secs);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let handle = handle.filter(|h| !h.is_empty()).ok_or("--handle is required")?;
    let text = if want_text { Some(read_text().map_err(|e| format!("the text could not be read: {e}"))?) } else { None };
    let event = event_for(&verb, origin, text).ok_or_else(|| format!("unknown verb {verb:?}"))?;
    Ok((event, handle, lock_wait))
}

/// Runs the command; the process's exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let (event, handle, lock_wait) = match parse(args, || {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map(|_| s)
    }) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("sotd work-state: {why}\n{USAGE}");
            return 2;
        }
    };
    match write_event(&handle, &event, lock_wait) {
        Ok(_) => 0,
        Err(StampError::Refused) => 2,
        Err(StampError::Registry(e)) => {
            eprintln!(
                "{}",
                match e {
                    EditError::Locked(line) => line,
                    EditError::NoHome => "FAILED: no comm home (SOT_COMM_HOME, HOME); stamp discarded".to_string(),
                    EditError::Unreadable(_) => "FAILED: the registry could not be read; stamp discarded".to_string(),
                    EditError::Unwritten => "FAILED: the registry could not be written; stamp discarded".to_string(),
                }
            );
            1
        }
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
