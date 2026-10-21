//! The daemon an install finds running (scripts/CLAUDE.md, Installers: "A live daemon is asked before it ends"): which
//! version it runs, asked of the target's own `sotd same-version`; what a restart would end, asked of `sotd update
//! repls`; and, when it cannot say, its own child processes. A daemon that runs and cannot be told is never taken for
//! none. The words here are the ones every question and every step uses.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::verdict::Stop;
use super::{cmd, procs};

/// What a run knows of the daemon on an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// It runs the target's version.
    Current,
    /// It runs another version.
    Other,
    /// It runs another version and a window holds it.
    Held,
    /// One runs and cannot be told its version.
    Unknown,
    /// None runs, whatever a stale socket or pipe file says.
    None,
}

impl State {
    #[cfg_attr(not(windows), allow(dead_code, reason = "the Windows end check names the state"))]
    pub fn word(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Other => "other",
            Self::Held => "held",
            Self::Unknown => "unknown",
            Self::None => "none",
        }
    }
}

/// The state `sotd same-version`'s exit code says: 0 the target's, 3 another, 4 another a window holds; any other answer
/// is `Unknown` when a daemon runs (`runs`) and `None` when none does.
pub(super) fn state_of(code: Option<i32>, runs: bool) -> State {
    match code {
        Some(0) => State::Current,
        Some(3) => State::Other,
        Some(4) => State::Held,
        _ if runs => State::Unknown,
        _ => State::None,
    }
}

/// The daemon on `endpoint` (`unix:<path>`, `pipe:<path>`), asked of `sotd`, and sotd's one line when it could not tell.
pub(super) fn state(sotd: &Path, endpoint: &str, runs: bool) -> (State, String) {
    state_within(sotd, endpoint, runs, Duration::from_secs(30))
}

/// [`state`] with the ask itself held to `bound` by the clock: an ask cut at its bound could not tell.
fn state_within(sotd: &Path, endpoint: &str, runs: bool, bound: Duration) -> (State, String) {
    let ran = cmd::output_within(Command::new(sotd).args(["same-version", "--endpoint", endpoint]), bound);
    let why = Some(ran.last_said()).filter(|s| !s.is_empty()).unwrap_or_else(|| "sotd same-version gave no reason".into());
    (state_of(ran.code, runs), why)
}

/// The state once the daemon has had up to `bound` by the clock to answer as the target: `Current` as soon as it does,
/// else what the last ask said. `held_ok` also ends the wait at `Held`.
pub(super) fn wait_current(sotd: &Path, endpoint: &str, bound: Duration, held_ok: bool, runs: bool) -> State {
    let deadline = Instant::now() + bound;
    loop {
        // Every ask ends by the deadline, and none starts after it.
        let left = deadline.saturating_duration_since(Instant::now());
        let (st, _) = state_within(sotd, endpoint, runs, left.max(Duration::from_millis(1)));
        let left = deadline.saturating_duration_since(Instant::now());
        if st == State::Current || (held_ok && st == State::Held) || left.is_zero() {
            return st;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
        if Instant::now() >= deadline {
            return st;
        }
    }
}

/// What a restart of the daemon would end: the rows with a live REPL, whether its Pluto server runs, and, when the
/// daemon could not say all of it, why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Work {
    pub rows: Vec<String>,
    pub pluto: bool,
    pub unsure: Option<String>,
}

/// `sotd update repls`' answer: its `repl <row>` and `pluto server` lines, and its exit code (0 none, 3 some, else it
/// could not say, with its reason on stderr).
pub(super) fn work_of(out: &str, err: &str, code: Option<i32>) -> Work {
    let rows = out.lines().filter_map(|l| l.strip_prefix("repl ")).map(|r| r.trim().to_string()).collect();
    let pluto = out.lines().any(|l| l.trim() == "pluto server");
    let unsure = match code {
        Some(0 | 3) => None,
        other => Some(
            err.lines()
                .chain(out.lines())
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("repl ") && *l != "pluto server")
                .map_or_else(|| format!("sotd update repls gave no reason (exit {})", other.map_or("none".into(), |c| c.to_string())), String::from),
        ),
    };
    Work { rows, pluto, unsure }
}

pub(super) fn work(sotd: &Path, endpoint: &str) -> Work {
    let ran = cmd::output(Command::new(sotd).args(["update", "repls", "--endpoint", endpoint]));
    work_of(&ran.out, &ran.err, ran.code)
}

/// What the daemon processes of the install at `prefix` run for a person, from their own process tree: `Some` with one
/// entry per direct child that is a REPL (`repl`) or the Pluto server (`pluto`), `None` when no daemon process of the
/// install is found, so nothing can be ruled out. The daemon is its program path, the sidecars its children by parent id.
pub(super) fn children(prefix: &Path) -> Option<Vec<&'static str>> {
    let all = procs::snapshot();
    let daemon = procs::daemon_pids(&all, prefix);
    if daemon.is_empty() {
        return None;
    }
    Some(all.iter().filter(|p| daemon.contains(&p.ppid)).filter_map(|p| child_kind(&p.args)).collect())
}

/// A daemon child's kind from its command line: the REPL sidecar runs ShipToolsRepl's shim, the Pluto server the
/// daemon's `julia/pluto/start.jl`.
pub(super) fn child_kind(args: &str) -> Option<&'static str> {
    if args.contains("ShipToolsRepl.run_shim") {
        return Some("repl");
    }
    let pluto = args.split_whitespace().any(|w| {
        let w = w.trim_matches('"').replace('\\', "/");
        w.ends_with("julia/pluto/start.jl")
    });
    pluto.then_some("pluto")
}

/// The work a restart ends, in words, after `lead` ("restarting it would end"); empty when there is none.
pub(super) fn work_words(rows: &[String], pluto: bool, lead: &str) -> String {
    let mut out = String::new();
    if !rows.is_empty() {
        out = format!("{lead} the REPLs of: {}", rows.join(", "));
    }
    if pluto {
        let p = "the Pluto server is running: restarting the daemon ends every notebook open in it";
        out = if out.is_empty() { p.to_string() } else { format!("{out}; {p}") };
    }
    out
}

/// What the question names: the REPL rows, else a REPL child the daemon could not name, then the Pluto server; `unsure`
/// adds that the daemon could not list all of it.
pub(super) fn ask_words(rows: &[String], repl_child: bool, pluto: bool, unsure: bool) -> String {
    let mut out = if !rows.is_empty() {
        format!("the REPLs of: {}", rows.join(", "))
    } else if repl_child {
        "a live REPL (the daemon could not name its row)".to_string()
    } else {
        String::new()
    };
    if pluto {
        let p = "the Pluto server and every notebook open in it";
        out = if out.is_empty() { p.to_string() } else { format!("{out}; {p}") };
    }
    if out.is_empty() {
        return "any REPL or Pluto notebook that is live".to_string();
    }
    if unsure {
        out.push_str(", and any other the daemon could not list");
    }
    out
}

/// What starting the daemon again would cost, for a step: the live work, and that the daemon could not say all of it.
#[cfg_attr(windows, allow(dead_code, reason = "the Unix steps for a daemon left on another version"))]
pub(super) fn rows_note(rows: &[String], pluto: bool, unknown: bool) -> String {
    let work = work_words(rows, pluto, "starting it again ends");
    match (unknown, work.is_empty()) {
        (true, false) => format!("{work}; it could not say the rest, so starting it again may end more"),
        (true, true) => "its live REPLs and Pluto server could not be listed, so starting it again may end some".to_string(),
        (false, _) => work,
    }
}

/// How a question asks: the daemon's live work, its child processes, the person at the console, and a note.
pub(super) struct Asks<'a> {
    pub live: &'a dyn Fn() -> Work,
    pub kids: &'a dyn Fn() -> Option<Vec<&'static str>>,
    pub ask: &'a dyn Fn(&str) -> bool,
    pub note: &'a dyn Fn(&str),
}

/// The Windows update's question: every way an update applies may stop or restart the daemon on `pipe`, so one that
/// runs (`state` not `None`), whatever its version, is asked first, yes at a console or `-RestartDaemon`
/// (`restart_flag`); a daemon that cannot say is judged by its own children. Changes nothing. What the restart would
/// end, for the update's steps; empty when nothing is live.
#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows update asks before it restarts the daemon"))]
pub(super) fn ask_before_update(state: State, pipe: &str, why: &str, restart_flag: bool, a: &Asks) -> Result<String, Stop> {
    if state == State::None {
        return Ok(String::new());
    }
    let work = (a.live)();
    let unsure = work.unsure.is_some() || state == State::Unknown;
    let kids = if unsure { (a.kids)() } else { None };
    if unsure && kids.as_ref().is_some_and(Vec::is_empty) && work.rows.is_empty() && !work.pluto {
        (a.note)(&format!("note: could not ask the daemon on {pipe} in full ({}); it has no REPL or Pluto child, so restarting it ends nothing live", work.unsure.as_deref().unwrap_or(why)));
        return Ok(String::new());
    }
    let live = live_text(state, pipe, &work, why);
    if live.is_empty() || restart_flag {
        return Ok(live);
    }
    let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
    if (a.ask)(&ask_words(&work.rows, repl, work.pluto || pluto, unsure)) {
        (a.note)("note: restarting the daemon as answered");
        return Ok(live);
    }
    println!("       Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); the rows and their agents keep running. Nothing under the install was changed. Ask whoever uses this computer, then run this again with -RestartDaemon.");
    Err(Stop::new(2, live))
}

/// What a restart of the daemon on `pipe` would end, as the reason of the question; empty when nothing is live.
fn live_text(state: State, pipe: &str, work: &Work, why: &str) -> String {
    let is = if state == State::Current { "is running" } else { "runs another version" };
    let rows = work.rows.join(", ");
    if state == State::Unknown {
        let rest = if rows.is_empty() { "; restarting it could end live REPLs and Pluto notebooks".to_string() } else { format!("; restarting it would end the REPLs of: {rows}") };
        return format!("a daemon runs on {pipe} and could not be told its version ({why}){rest}");
    }
    if let Some(reason) = &work.unsure {
        let rest = if rows.is_empty() { String::new() } else { format!(", and restarting it would end the REPLs of: {rows}") };
        return format!("could not ask the daemon on {pipe} in full: it {is}{rest}; sotd update repls: {reason}");
    }
    let t = work_words(&work.rows, work.pluto, "restarting it would end");
    if t.is_empty() { t } else { format!("the daemon on {pipe} {is}, and {t}") }
}

/// One answer, asked in place at the console: "Restart the daemon? This ends WORK [y/N]". False, asking nothing, where
/// there is no console. Unix asks on `/dev/tty` (it works under `curl | bash`; `SOT_INSTALL_TTY` names another file for
/// a test); Windows on standard input when it is a console.
pub(super) fn ask_restart(work: &str) -> bool {
    let question = format!("Restart the daemon? This ends {work} [y/N] ");
    let answer = read_answer(&question);
    matches!(answer.as_deref().map(str::trim), Some("y" | "Y" | "yes" | "Yes" | "YES"))
}

#[cfg(unix)]
#[allow(clippy::disallowed_methods, reason = "a person's answer at the console, not a child's output")]
pub(super) fn read_answer(question: &str) -> Option<String> {
    use std::io::{BufRead as _, Write as _};
    let tty = std::env::var_os("SOT_INSTALL_TTY").unwrap_or_else(|| "/dev/tty".into());
    let mut out = std::fs::OpenOptions::new().write(true).open(&tty).ok()?;
    let input = std::fs::File::open(&tty).ok()?;
    out.write_all(question.as_bytes()).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(input).read_line(&mut line).ok()?;
    Some(line)
}

#[cfg(windows)]
#[allow(clippy::disallowed_methods, reason = "a person's answer at the console, not a child's output")]
pub(super) fn read_answer(question: &str) -> Option<String> {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
