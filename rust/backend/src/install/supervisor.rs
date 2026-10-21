//! The Windows window's supervisor (`launch-sot.ps1`) as its log tells it: the launcher code id its last start line
//! names, and how far a converge has come. Plain text here, so it is tested on every system; `windows.rs` reads the
//! files. The words are the launcher's and the apply's (`install::tests::the_windows_install_reads_the_words_the_launcher_writes`).

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

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
