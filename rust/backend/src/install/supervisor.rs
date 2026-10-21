//! The Windows window's supervisor (`launch-sot.ps1`) as its log tells it: the launcher code id its last start line
//! names, how far a converge has come, the wait for it, and which way an update applies. Plain files and text here, so
//! it is tested on every system; `windows.rs` finds the supervisor and asks it. The words are the launcher's and the
//! apply's (docs/integration.md, the resident-supervisor record); no test reads both sides.

use std::path::Path;
use std::time::{Duration, Instant};

/// The launcher code id the supervisor `pid` logged at its last start (`pid=<pid>  supervisor start ... code=<id>`), or
/// empty when its start line names none (an older launcher, or `code=unknown`), which never re-invokes after a converge.
#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows install reads the supervisor"))]
pub(super) fn start_code(log: &str, pid: u32) -> String {
    let me = format!("pid={pid}");
    log.lines()
        .filter(|l| l.split_once(&me).is_some_and(|(_, rest)| rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("supervisor start")))
        .last()
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("code=")))
        .filter(|c| *c != "unknown")
        .unwrap_or("")
        .to_string()
}

/// How far a converge to `tag` has come, from what the supervisor's log gained since it was asked.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Converge {
    Waiting,
    /// The apply said `APPLIED <tag>` and the launch then said `status: DONE`.
    Done,
    /// The apply put the previous binaries back.
    Restored,
}

#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows install waits for a converge"))]
pub(super) fn converge_state(text: &str, tag: &str) -> Converge {
    if let (Some(applied), Some(done)) = (text.rfind(&format!("sot-apply: APPLIED {tag}")), text.rfind("status: DONE")) {
        if done > applied {
            return Converge::Done;
        }
    }
    if text.contains("restoring previous binaries") {
        return Converge::Restored;
    }
    Converge::Waiting
}

/// Wait up to `bound` by the clock for the converge to `tag` the supervisor was asked for, reading what its `log`
/// gained past byte `start`; `note` hears the wait every 30 s. Every end removes the converge `sentinel`, since one
/// nobody consumed would make the next window converge the moment it starts. Why not, when it did not apply.
#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows install waits for a converge"))]
pub(super) fn wait_converge(log: &Path, start: u64, sentinel: &Path, tag: &str, bound: Duration, note: &dyn Fn(u64)) -> Result<(), String> {
    let t0 = Instant::now();
    let mut last_note = t0;
    let outcome = loop {
        let text = std::fs::read(log).map(|b| String::from_utf8_lossy(&b[usize::try_from(start).unwrap_or(0).min(b.len())..]).into_owned()).unwrap_or_default();
        match converge_state(&text, tag) {
            Converge::Done => break Ok(()),
            Converge::Restored => break Err(format!("the supervisor's apply did not complete: see {}", log.display())),
            Converge::Waiting => {}
        }
        let left = bound.saturating_sub(t0.elapsed());
        if left.is_zero() {
            break Err(format!("the supervisor did not report {tag} applied within {} s: see {}", bound.as_secs(), log.display()));
        }
        if last_note.elapsed() >= Duration::from_secs(30) {
            last_note = Instant::now();
            note(t0.elapsed().as_secs());
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    };
    let _ = std::fs::remove_file(sentinel);
    outcome
}

/// The one way a staged update applies, by what runs here.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Way {
    /// A supervisor that logs its launcher code id: it is asked to converge, and the run waits.
    Converge,
    /// An older supervisor, or a window with none: the update stays armed until the person quits and starts it.
    Quit,
    /// Nothing runs and this session is not at the desk (an ssh logon): the update stays armed for a start there.
    Desk,
    /// Nothing runs and this is a desktop session: the update applies here.
    Here,
}

/// The way for a resident supervisor's launcher code id (`None` when no supervisor runs, empty when it logs none), a
/// window running without one, and this process's logon session (0 for an ssh logon), asked only when nothing runs.
#[cfg_attr(not(windows), allow(dead_code, reason = "the Windows install chooses how an update applies"))]
pub(super) fn way(supervisor_code: Option<&str>, window_only: bool, session: &dyn Fn() -> Option<u32>) -> Way {
    match supervisor_code {
        Some(code) if !code.is_empty() => Way::Converge,
        Some(_) => Way::Quit,
        None if window_only => Way::Quit,
        None if session() == Some(0) => Way::Desk,
        None => Way::Here,
    }
}
