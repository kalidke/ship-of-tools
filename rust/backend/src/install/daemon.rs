//! The daemon an install finds running (scripts/CLAUDE.md, Installers: "A live daemon is asked before it ends"): which
//! version it runs, asked of the target's own `sotd same-version`; what a restart would end, asked of `sotd update
//! repls`; and, when it cannot say, its own child processes. A daemon that runs and cannot be told is never taken for
//! none. The words here are the ones every question and every step uses.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use sot_platform::host::env::Env;

use super::observe::{self, Fact, Machine};
use super::procs;
use super::verdict::Stop;

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

/// What `sotd same-version` answered (rust/backend/src/topology/same_version.rs:6-11): its exit code (0 the target's,
/// 3 another, 4 another a window holds, 2 it cannot be told) and its line (the version it runs when another, the reason
/// when it cannot be told).
pub(super) type Said = (i32, String);

/// Whether a daemon may run, asked within the bound it is given.
pub(super) type Runs<'a> = &'a dyn Fn(Duration) -> Fact<bool>;

/// The state `sotd same-version`'s answer says, and its line or why it gave none. A daemon that said it cannot be told,
/// or gave no answer, is `Unknown` when one may run and `None` when none does; whether one runs is asked only then
/// (`runs`, within `bound`), and a `runs` that cannot say presumes one does, its reason kept. `None`'s line is empty when
/// the daemon answered, and the ask's reason when it gave no answer: the caller's [`none_clause`].
pub(super) fn state_of(said: Fact<Said>, runs: Runs, bound: Duration) -> (State, String) {
    let unknown = |why: String| runs(bound).read(|runs| if runs { (State::Unknown, why.clone()) } else { (State::None, String::new()) }, |listing| (State::Unknown, format!("{listing}; {why}")));
    said.read(
        |(code, line)| match code {
            0 => (State::Current, line),
            3 => (State::Other, line),
            4 => (State::Held, line),
            _ => unknown(line),
        },
        |why| {
            let why = why.to_string();
            match unknown(why.clone()) {
                (State::None, _) => (State::None, why),
                seen => seen,
            }
        },
    )
}

/// The clause a `None` carries when the daemon on `endpoint` gave no answer and the listing showed none of the
/// install's: the ask's reason, for the verdict.
pub(super) fn none_clause(endpoint: &str, (state, why): (State, &str)) -> Option<String> {
    (state == State::None && !why.is_empty()).then(|| format!("the daemon on {endpoint} could not be asked ({why}); no daemon of this install was listed, so none runs"))
}

/// The daemon on `endpoint` (`unix:<path>`, `pipe:<path>`), asked of `sotd`, and its version when different, else
/// why it could not tell.
pub(super) fn state(m: &dyn Machine, sotd: &Path, endpoint: &str, runs: Runs) -> (State, String) {
    state_within(m, sotd, endpoint, runs, Duration::from_secs(30), || observe::ASK_BOUND)
}

/// [`state`] with the ask itself held to `bound` by the clock, and a listing it needs to the bound `left` gives once
/// the ask has ended: an ask cut at its bound could not tell. The reader is same-version's grammar
/// (rust/backend/src/topology/same_version.rs:6-11, 50-63): another version is named on stdout, never empty, and
/// `cannot be told` is exit 2 (exit 1 from a `sotd` that predates the command) with its reason on stderr.
fn state_within(m: &dyn Machine, sotd: &Path, endpoint: &str, runs: Runs, bound: Duration, left: impl Fn() -> Duration) -> (State, String) {
    let said = observe::ask(m, Command::new(sotd).args(["same-version", "--endpoint", endpoint]), bound, |a| match a.exit {
        0 => Ok((0, a.last_said())),
        3 | 4 if !a.first_line().is_empty() => Ok((a.exit, a.first_line())),
        3 | 4 => Err(observe::Why::Malformed(format!("another version with no version named (exit {})", a.exit))),
        1 | 2 => Ok((2, Some(a.last_said()).filter(|l| !l.is_empty()).unwrap_or_else(|| format!("`sotd same-version` exit {} with no reason", a.exit)))),
        _ => Err(a.failed()),
    });
    state_of(said, runs, left())
}

/// The state once the daemon has had up to `bound` by the clock to answer as the target: `Current` as soon as it does,
/// else what the last ask said, with its reason, so no caller asks again past the bound. `held_ok` also ends the wait
/// at `Held`.
pub(super) fn wait_current(m: &dyn Machine, sotd: &Path, endpoint: &str, bound: Duration, held_ok: bool, runs: Runs) -> (State, String) {
    let deadline = Instant::now() + bound;
    loop {
        // Every ask, the listing it may need included, ends by the deadline or its own bound, the smaller, and none
        // starts after the deadline.
        let left = deadline.saturating_duration_since(Instant::now());
        let (st, why) = state_within(m, sotd, endpoint, runs, ask_slice(left), || ask_slice(deadline.saturating_duration_since(Instant::now())));
        let left = deadline.saturating_duration_since(Instant::now());
        if st == State::Current || (held_ok && st == State::Held) || left.is_zero() {
            return (st, why);
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
        if Instant::now() >= deadline {
            return (st, why);
        }
    }
}

/// The bound of one ask in a wait with `left` of its deadline: the smaller of the two, so a long configured wait never
/// lengthens one observation past [`observe::ASK_BOUND`].
fn ask_slice(left: Duration) -> Duration {
    left.min(observe::ASK_BOUND).max(Duration::from_millis(1))
}

/// What a restart of the daemon would end: the rows with a live REPL, whether its Pluto server runs, and, when the
/// daemon could not say all of it, why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Work {
    pub rows: Vec<String>,
    pub pluto: bool,
    pub unsure: Option<String>,
}

/// `sotd update repls`' answer: exit 0 with no line (nothing live), exit 3 with its `repl <row>` and `pluto server`
/// lines and nothing else, or another exit (it could not say all of it, its reason on stderr, the lines it gave kept).
/// Exit 0 with lines, or exit 3 with no line or a line that is neither, does not read.
pub(super) fn work_of(a: &observe::Answer) -> Result<Work, observe::Why> {
    let lines: Vec<&str> = a.out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let rows: Vec<String> = lines.iter().filter_map(|l| l.strip_prefix("repl ")).map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    let pluto = lines.contains(&"pluto server");
    let read = rows.len() + usize::from(pluto) == lines.len();
    match a.exit {
        0 if lines.is_empty() => Ok(Work::default()),
        3 if read && !lines.is_empty() => Ok(Work { rows, pluto, unsure: None }),
        0 | 3 => Err(observe::Why::Malformed(format!("no reading of its live work (exit {}: '{}')", a.exit, lines.join("; ")))),
        exit => {
            let said = a.err.lines().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| format!("sotd update repls gave no reason (exit {exit})"), String::from);
            Ok(Work { rows, pluto, unsure: Some(said) })
        }
    }
}

/// What a restart would end, asked of the daemon within [`observe::ASK_BOUND`]; an ask that gave no answer, or one that
/// does not read, is `unsure` with its reason, so a caller that cannot be told is asked about, never told "nothing".
pub(super) fn work(m: &dyn Machine, sotd: &Path, endpoint: &str) -> Work {
    observe::ask(m, Command::new(sotd).args(["update", "repls", "--endpoint", endpoint]), observe::ASK_BOUND, work_of)
        .read(|w| w, |why| Work { unsure: Some(why.to_string()), ..Work::default() })
}

/// What the daemon processes of the install at `prefix` run for a person, from their own process tree: `Some` with one
/// entry per direct child that is a REPL (`repl`) or the Pluto server (`pluto`), `None` when no daemon process of the
/// install is found, so nothing can be ruled out; unknown, with its reason, when the listing gave none. The daemon is
/// its program path, the sidecars its children by parent id.
pub(super) fn children(m: &dyn Machine, prefix: &Path) -> Fact<Option<Vec<&'static str>>> {
    children_in(procs::snapshot(m), prefix)
}

pub(super) fn children_in(all: Fact<Vec<procs::Proc>>, prefix: &Path) -> Fact<Option<Vec<&'static str>>> {
    all.map(|all| {
        let daemon = procs::daemon_pids(&all, prefix);
        (!daemon.is_empty()).then(|| all.iter().filter(|p| daemon.contains(&p.ppid)).filter_map(|p| child_kind(&p.args)).collect())
    })
}

/// A children reading as the questions use it: the kinds, or nothing ruled out, and why not when the listing gave no
/// answer.
pub(super) fn kids_of(kids: Fact<Option<Vec<&'static str>>>) -> (Option<Vec<&'static str>>, Option<String>) {
    kids.read(|k| (k, None), |why| (None, Some(why.to_string())))
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

/// How a question asks: the daemon's live work, its child processes, the person at the console, a note, and a clause
/// of the verdict for each unknown the run goes past.
pub(super) struct Asks<'a> {
    pub live: &'a dyn Fn() -> Work,
    pub kids: &'a dyn Fn() -> Fact<Option<Vec<&'static str>>>,
    pub ask: &'a dyn Fn(&str) -> bool,
    pub note: &'a dyn Fn(&str),
    pub clause: &'a dyn Fn(String),
}

/// `same-version` prints the running daemon's version only when it differs (Other or Held); otherwise its line is a
/// diagnostic. A 0.7 daemon retires the row layout a 0.6 daemon left, ending each old agent once.
pub(super) fn retires_agents(state: State, version: &str) -> bool {
    matches!(state, State::Other | State::Held) && version.starts_with("0.6.")
}

/// What an update does about the daemon before it applies here, once nothing else runs: apply directly when none runs,
/// stop it first when one does, and hold (stop nothing, leave the update armed) when one runs and cannot be told, since
/// stopping what is not known is not safe.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows update applies here"))]
pub(super) enum Apply {
    Direct,
    StopFirst,
    Hold,
}

#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows update applies here"))]
pub(super) fn apply_here(state: State) -> Apply {
    match state {
        State::None => Apply::Direct,
        State::Unknown => Apply::Hold,
        State::Current | State::Other | State::Held => Apply::StopFirst,
    }
}

/// What happens to agents when the daemon is replaced, using the running version, not the install record's.
pub(super) fn agents_note(state: State, version: &str) -> &'static str {
    if retires_agents(state, version) {
        "this update from 0.6 to 0.7 ends every agent once; each conversation comes back when you use /resume in its row."
    } else {
        "the rows and their agents keep running."
    }
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
    let retires = retires_agents(state, why);
    if retires {
        (a.note)(&format!("note: {}", agents_note(state, why)));
    }
    let work = (a.live)();
    let unsure = work.unsure.is_some() || state == State::Unknown;
    let (kids, kids_why) = if unsure { kids_of((a.kids)()) } else { (None, None) };
    // What the run could not be told, for the verdict when it goes on.
    let untold = [(state == State::Unknown).then(|| why.to_string()), work.unsure.clone(), kids_why.clone()].into_iter().flatten().collect::<Vec<_>>().join("; ");
    if unsure && kids.as_ref().is_some_and(Vec::is_empty) && work.rows.is_empty() && !work.pluto {
        let ends = if retires { "no live REPL or Pluto notebook" } else { "nothing live" };
        (a.note)(&format!("note: could not ask the daemon on {pipe} in full ({untold}); it has no REPL or Pluto child, so restarting it ends {ends}"));
        (a.clause)(format!("the daemon on {pipe} could not be asked in full ({untold}); its own children showed nothing live"));
        return Ok(String::new());
    }
    let mut live = live_text(state, pipe, &work, why);
    if let Some(listing) = &kids_why {
        live = format!("{live}; its children could not be listed ({listing})");
    }
    if live.is_empty() || restart_flag {
        if unsure && restart_flag {
            (a.clause)(format!("the daemon on {pipe} could not be asked in full ({untold}); restarted as -RestartDaemon asks"));
        }
        return Ok(live);
    }
    let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
    if (a.ask)(&ask_words(&work.rows, repl, work.pluto || pluto, unsure)) {
        (a.note)("note: restarting the daemon as answered");
        if unsure {
            (a.clause)(format!("the daemon on {pipe} could not be asked in full ({untold}); restarted as answered at the console"));
        }
        return Ok(live);
    }
    let agents = agents_note(state, why);
    println!("       Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); {agents} Nothing under the install was changed. Ask whoever uses this computer, then run this again with -RestartDaemon.");
    Err(Stop::new(2, live))
}

/// What a restart of the daemon on `pipe` would end, as the reason of the question; empty when nothing is live.
fn live_text(state: State, pipe: &str, work: &Work, why: &str) -> String {
    let is = if state == State::Current { "is running" } else { "runs another version" };
    let rows = work.rows.join(", ");
    if state == State::Unknown {
        let rest = if rows.is_empty() { "; restarting it could end live REPLs and Pluto notebooks".to_string() } else { format!("; restarting it would end the REPLs of: {rows}") };
        let unsure = work.unsure.as_ref().map_or_else(String::new, |reason| format!("; sotd update repls: {reason}"));
        return format!("a daemon runs on {pipe} and could not be told its version ({why}){unsure}{rest}");
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
pub(super) fn ask_restart(env: &Env, work: &str) -> bool {
    let question = format!("Restart the daemon? This ends {work} [y/N] ");
    let answer = read_answer(env, &question);
    matches!(answer.as_deref().map(str::trim), Some("y" | "Y" | "yes" | "Yes" | "YES"))
}

#[cfg(unix)]
#[allow(clippy::disallowed_methods, reason = "a person's answer at the console, not a child's output")]
pub(super) fn read_answer(env: &Env, question: &str) -> Option<String> {
    use std::io::{BufRead as _, Write as _};
    let tty = env.get_nonempty("SOT_INSTALL_TTY").map_or_else(|| "/dev/tty".into(), std::ffi::OsStr::to_os_string);
    let mut out = std::fs::OpenOptions::new().write(true).open(&tty).ok()?;
    let input = std::fs::File::open(&tty).ok()?;
    out.write_all(question.as_bytes()).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(input).read_line(&mut line).ok()?;
    Some(line)
}

#[cfg(windows)]
#[allow(clippy::disallowed_methods, reason = "a person's answer at the console, not a child's output")]
pub(super) fn read_answer(_env: &Env, question: &str) -> Option<String> {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("{question}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
