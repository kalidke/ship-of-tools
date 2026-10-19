//! What a failed case leaves behind: the daemon logs of the case, the answers its routes got and, at a timeout, the
//! daemon's descendants as they stood. A case that passes keeps nothing. The record lives in
//! `$SOT_LIFETIME_KEEP_DIR/<case>-<pid>/`, else `<temp dir>/sot-lifetime-keep/<case>-<pid>/`, and its path and the tail of
//! each log are printed to the case's output, so a run without the folder (a hosted one) still shows the cause. The one
//! signal this file sends is SIGUSR1 to a `julia` process found by walking `/proc` parent links from the daemon the case
//! holds, which makes Julia print every thread's stack to its stderr (the daemon logs it); nothing else is ever signalled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The log lines printed per daemon log.
const TAIL_LINES: usize = 60;

/// The case's name: the test thread's, which a current-thread runtime shares with the tasks it spawns.
fn case_name() -> String {
    std::thread::current().name().unwrap_or("case").to_string()
}

/// The answers each case's routes got, and the daemons each case holds, by case name.
static NOTES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static DAEMONS: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());

/// Record one line of what a route answered (or that it timed out) for this case.
pub fn note(line: String) {
    NOTES.lock().unwrap().push((case_name(), line));
}

/// The case holds this daemon: a timeout lists its descendants. The first call arms the timeout hook.
pub fn note_daemon(pid: i32) {
    DAEMONS.lock().unwrap().push((case_name(), pid));
    *crate::support::TIMEOUT_HOOK.lock().unwrap() = Some(at_timeout);
}

/// `poll_until` and `call_long` call this before they panic: the descendants of the case's daemons, SIGUSR1 to the
/// `julia` ones, six seconds, and the descendants again, all noted.
pub fn at_timeout(what: &str) {
    let name = case_name();
    let daemons: Vec<i32> = DAEMONS
        .lock()
        .unwrap()
        .iter()
        .filter(|(case, _)| *case == name)
        .map(|(_, pid)| *pid)
        .collect();
    for daemon in daemons {
        let before = descendants(daemon);
        note(format!(
            "timeout: {what}\ndescendants of daemon {daemon} before SIGUSR1:\n{}",
            list(&before)
        ));
        for (pid, _) in before.iter().filter(|(_, comm)| comm.as_str() == "julia") {
            // SAFETY: SIGUSR1 to a julia process this daemon's own descendant walk found.
            unsafe { libc::kill(*pid, libc::SIGUSR1) };
        }
        std::thread::sleep(Duration::from_secs(6));
        note(format!(
            "descendants of daemon {daemon} six seconds after SIGUSR1:\n{}",
            list(&descendants(daemon))
        ));
    }
}

/// The processes below `root` by parent links: each pid with its command name.
fn descendants(root: i32) -> Vec<(i32, String)> {
    let mut parents: HashMap<i32, Vec<i32>> = HashMap::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        if let Some(parent) = crate::guard::parent_of(pid) {
            parents.entry(parent).or_default().push(pid);
        }
    }
    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for child in parents.get(&pid).into_iter().flatten() {
            let comm = std::fs::read_to_string(format!("/proc/{child}/comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            found.push((*child, comm));
            queue.push(*child);
        }
    }
    found.sort();
    found
}

/// One line per process: pid, state, wchan and command line.
fn list(procs: &[(i32, String)]) -> String {
    let mut out = String::new();
    for (pid, comm) in procs {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit_once(')').map(|(_, rest)| rest.trim_start().chars().next()))
            .flatten()
            .unwrap_or('?');
        let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
        out.push_str(&format!(
            "  {pid} {comm} state={state} wchan={wchan} cmd={}\n",
            crate::routes::command_line(*pid).trim_end()
        ));
    }
    out
}

/// The folder a failed case's record goes to.
fn keep_folder() -> PathBuf {
    let base = std::env::var_os("SOT_LIFETIME_KEEP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("sot-lifetime-keep"));
    base.join(format!("{}-{}", case_name().replace(':', "_"), std::process::id()))
}

/// A case that is panicking keeps its record: every daemon log in `case_dir` and the notes, in a folder that outlives the
/// case; the folder's path and each log's tail are printed. A case that is not panicking keeps nothing.
pub fn keep_if_failed(case_dir: &Path) {
    if !std::thread::panicking() {
        return;
    }
    let keep = keep_folder();
    if std::fs::create_dir_all(&keep).is_err() {
        eprintln!("lifetime record: cannot create {}", keep.display());
        return;
    }
    let name = case_name();
    let notes: String = NOTES
        .lock()
        .unwrap()
        .iter()
        .filter(|(case, _)| *case == name)
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let _ = std::fs::write(keep.join("notes.txt"), &notes);
    eprintln!("lifetime record kept in {}", keep.display());
    for entry in std::fs::read_dir(case_dir).into_iter().flatten().flatten() {
        let file = entry.file_name();
        let Some(text) = file.to_str().filter(|n| n.starts_with("daemon-") && n.ends_with(".log")) else {
            continue;
        };
        #[allow(clippy::disallowed_methods, reason = "a data copy of a log, no program is run from it")]
        let _ = std::fs::copy(entry.path(), keep.join(text));
        let log = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        eprintln!(
            "--- last {TAIL_LINES} lines of {text} ---\n{}",
            lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
        );
    }
    eprintln!("--- notes ---\n{notes}");
}
