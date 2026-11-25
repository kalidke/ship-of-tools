//! The Codex wake: a row with unread mail is asked to poll through its own Codex queue, never through its terminal.
//! Nothing here can type: this module names no terminal client.
//!
//! [`Rows::tick`] is the tick's whole hand in it. Each tick it takes the row's finished look, if one is finished, and
//! starts the next on a blocking thread; it never waits for one, so a queue command that hangs holds no other row. A
//! look ([`look`]) reads the inbox by the wake's own unread rule, and for mail not yet asked for ([`verdict`]) it
//! requires the row Ready, reads the conversation id the row's session named ([`binding`]) and runs the queue command
//! once with the fixed wake line as its one message. What was asked lives in the tick task's memory ([`Asked`]): the
//! conversation, the inbox's line count then, and whether the request was refused. After an accepted or an unclear
//! request nothing more goes until every unread line is newer than that count (the mail it announced was read and more
//! came) or the binding names another conversation; there is no reminder, since a second request to an open session
//! is a second turn. After a definite refusal the next request waits [`REPEAT_AFTER`], whatever mail comes. With no
//! conversation to name nothing runs, and that is logged once until the inbox reads empty. The memory dies with the
//! daemon, so a restarted one asks once more for mail still unread.
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

use std::collections::HashSet;
use std::ffi::OsString;

use super::*;

/// The launcher and Codex's queue command together. The command returned in under a second in every trial; the
/// launcher asks the binary's help first, and both start a runtime.
pub(super) const QUEUE_WITHIN: Duration = Duration::from_secs(10);
/// The launcher's status for a refusal of its own, and the start of its line.
const NO_QUEUE_STATUS: i32 = 69;
const NO_QUEUE_LINE: &str = "ccx: no queue:";
/// The longest piece of a child's output kept in an outcome.
const SAID_MAX: usize = 200;

/// What the wake last asked of a Codex row's conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    thread: String,
    /// The inbox's complete-line count when the request went: the mail it announced.
    line: u64,
    at: Instant,
    /// A definite refusal: nothing was stored.
    refused: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Nothing unread, and no refusal to remember: forget the handle.
    Clear,
    Hold,
    Ask,
}

/// `thread` reads the conversation the binding names now, when it names one; it is asked only for unread mail that was
/// already announced. A refusal is the launcher's or the binary's, not the mail's, so it holds through a read inbox.
fn verdict(s: &Scan, prior: Option<&Asked>, thread: &dyn Fn() -> Option<String>, now: Instant) -> Verdict {
    match prior {
        Some(p) if p.refused && now.duration_since(p.at) < REPEAT_AFTER => Verdict::Hold,
        _ if s.unread == 0 => Verdict::Clear,
        Some(p) if !p.refused && s.fresh < s.unread && thread().map_or(true, |t| t == p.thread) => Verdict::Hold,
        _ => Verdict::Ask,
    }
}

/// One look at a Codex row.
#[derive(Debug, PartialEq, Eq)]
enum Look {
    Clear,
    Skip,
    /// Unread mail, and why there is no conversation to ask.
    Unbound(&'static str),
    Asked(Asked),
}

/// The longest binding file read: an id, its newline, and room to see that there is more.
const BINDING_MAX: u64 = 64;

/// The conversation the row's session last named in its binding file (agents' guarantee 7), or why there is none.
/// Only a regular file of at most [`BINDING_MAX`] bytes is read, never waiting (platform's `read_regular`): a fifo, a
/// device, a directory or a large file at that name is no binding and costs the look nothing. A link at the name is
/// refused first; the check and the open are two steps, so a name replaced by a link between them is read through it,
/// still as a small regular file (messaging's contract, Waking, has the limit).
fn binding(state_dir: &Path) -> Result<String, &'static str> {
    let file = state_dir.join(crate::agents::env::CODEX_THREAD_FILE);
    if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("its binding file is a link");
    }
    match sot_platform::host::read_regular(&file, BINDING_MAX) {
        Ok(text) => Some(text.strip_suffix('\n').unwrap_or(&text)).filter(|id| is_id(id)).map(str::to_string).ok_or("its binding file holds no conversation id"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("its session has named no conversation in this run"),
        Err(_) => Err("its binding file is not a small regular file"),
    }
}

/// Whether a Codex session in `state_dir` can never name its conversation: its leg was not launched with the row's hooks
/// (no marker, agents' `leg_hooked`) and its binding names none. The marker is read first, one `stat`; a hooked leg needs
/// nothing more. Rows' listing marks a Ready such session (`not_woken_by_mail`), and the wake's log says the same for
/// mail waiting on it.
pub(crate) fn not_woken_by_mail(state_dir: &Path) -> bool {
    !crate::agents::selection::leg_hooked(state_dir) && binding(state_dir).is_err()
}

/// Blocking: a status round trip to the row and, when a request goes, the queue command's whole run. The command is
/// the launcher a Codex leg runs (agents' recipe), in the row's folder, under the daemon's environment without the
/// nesting variables: what a leg inherits before its row's pairs, so the same login (a Codex row has no named account)
/// and no mail identity or selection of the row's.
fn look(home: &Path, handle: &str, state_dir: &Path, cwd: &Path, prior: Option<Asked>) -> Look {
    let s = scan(home, handle, prior.as_ref().map_or(0, |p| p.line));
    let judge = |thread: &dyn Fn() -> Option<String>| verdict(&s, prior.as_ref(), thread, Instant::now());
    match judge(&|| binding(state_dir).ok()) {
        Verdict::Clear => return Look::Clear,
        Verdict::Hold => return Look::Skip,
        Verdict::Ask => {}
    }
    // Only a Ready row is asked. Its binding file was removed as its leg started, and a row reads Ready only once that
    // leg has lived 2 s, so an id read after this is the running leg's own, never an earlier run's.
    if crate::rows::run::probe::phase_of(state_dir) != crate::rows::workspace::Phase::Ready.as_wire_str() {
        return Look::Skip;
    }
    let thread = match binding(state_dir) {
        Ok(thread) => thread,
        Err(why) => return Look::Unbound(why),
    };
    if judge(&|| Some(thread.clone())) != Verdict::Ask {
        return Look::Skip;
    }
    // A test's hold point: this look is about to run its command.
    sot_platform::hold_point!("comm.wake.codex.queue", handle);
    let env: Vec<_> = std::env::vars_os().filter(|(k, _)| !crate::agents::env::NESTING_ENV_VARS_TO_SCRUB.iter().any(|v| k.to_str() == Some(v))).collect();
    let queued = match crate::agents::argv::agent_argv("codex", None) {
        Ok(argv) => queue(sot_containment::process(), Path::new(&argv[0]), &env, cwd, &thread, WAKE_LINE, QUEUE_WITHIN),
        Err(e) => Queued::Refused(e),
    };
    match &queued {
        Queued::Accepted => tracing::info!(handle, thread, "comm wake: asked the Codex row's conversation to poll its mail; the request is queued and may run later"),
        Queued::Refused(why) => tracing::warn!(handle, why, "comm wake: the Codex queue command was refused before it stored anything; tried again in {REPEAT_AFTER:?}"),
        Queued::Unclear(why) => tracing::warn!(handle, thread, why, "comm wake: the Codex queue command's outcome is unclear; nothing more is queued for this mail"),
    }
    Look::Asked(Asked { thread, line: s.total, at: Instant::now(), refused: matches!(queued, Queued::Refused(_)) })
}

/// The tick task's memory of its Codex rows.
#[derive(Default)]
pub(super) struct Rows {
    asked: HashMap<String, Asked>,
    /// Handles whose missing conversation is logged for this unread episode.
    unbound: HashSet<String>,
    flying: HashMap<String, tokio::task::JoinHandle<Look>>,
}

impl Rows {
    pub(super) async fn tick(&mut self, home: &Path, handle: &str, state_dir: PathBuf, cwd: PathBuf) {
        if self.flying.get(handle).is_some_and(|look| !look.is_finished()) {
            return;
        }
        if let Some(look) = self.flying.remove(handle) {
            self.settle(handle, look.await.unwrap_or(Look::Skip));
            // A test's hold point, as for a row the tick types into: this row's look is settled (counted).
            sot_platform::hold_point_async!("comm.wake.looked", handle);
        }
        let (home, h, prior) = (home.to_path_buf(), handle.to_string(), self.asked.get(handle).cloned());
        self.flying.insert(handle.to_string(), tokio::task::spawn_blocking(move || look(&home, &h, &state_dir, &cwd, prior)));
    }

    fn settle(&mut self, handle: &str, look: Look) {
        match look {
            Look::Clear => {
                self.asked.remove(handle);
                self.unbound.remove(handle);
            }
            Look::Skip => {}
            Look::Unbound(why) => {
                if self.unbound.insert(handle.to_string()) {
                    tracing::info!(handle, why, "comm wake: a Codex row has unread mail and no conversation to ask; the mail waits for the row's next poll");
                }
            }
            Look::Asked(asked) => {
                self.asked.insert(handle.to_string(), asked);
            }
        }
    }
}

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
