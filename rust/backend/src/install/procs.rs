//! This account's processes, as an install reads them to find the install's daemon and windows by program path and a
//! daemon's sidecars by parent id, never by a pattern over every command line (ADR 0050). Unix asks `ps` for the
//! kernel's name of each program (`ucomm`, which macOS does not cut from argv[0]); Windows asks CIM's Win32_Process.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::cmd;

#[derive(Debug, Clone, Default)]
pub(super) struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// Unix: the kernel's name for the program file. Windows: the image's file name.
    pub name: String,
    /// The program path: Unix argv[0], Windows the image path.
    pub exe: String,
    pub args: String,
}

/// This account's processes, or why they could not be listed: a listing that did not start, was cut at its bound,
/// ended badly or gave no process is an `Err`, never an empty list (this process is always one of them), so no caller
/// reads "could not list" as "none runs".
#[cfg(unix)]
pub(super) fn snapshot() -> Result<Vec<Proc>, String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    list(Command::new("ps").args(["-u", &uid.to_string(), "-ww", "-o", "pid=", "-o", "ppid=", "-o", "ucomm=", "-o", "args="]), cmd::ASK_BOUND, parse_ps)
}

/// The listing `command` prints within `bound`, read by `parse`: `None` when the text is not a process listing.
fn list(command: &mut Command, bound: Duration, parse: impl Fn(&str) -> Option<Vec<Proc>>) -> Result<Vec<Proc>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let ran = cmd::ask_within(command, bound).map_err(|why| format!("the process listing gave no answer: {why}"))?;
    if !ran.ok() {
        return Err(format!("{program} ended with exit {}: {}", ran.code.unwrap_or_default(), ran.last_said()));
    }
    parse(&ran.out).filter(|all| !all.is_empty()).ok_or_else(|| format!("{program} printed no valid process list"))
}

/// `ps -o pid= -o ppid= -o ucomm= -o args=` lines; `None` when a line is not one, since a listing with a row missing
/// would say a process is absent.
#[cfg_attr(windows, allow(dead_code, reason = "Unix reads its processes from ps"))]
pub(super) fn parse_ps(text: &str) -> Option<Vec<Proc>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let name = words.next()?.to_string();
            let args: Vec<&str> = words.collect();
            Some(Proc { pid, ppid, name, exe: args.first().map(|s| (*s).to_string()).unwrap_or_default(), args: args.join(" ") })
        })
        .collect()
}

/// This account's processes, or why they could not be listed; see the Unix listing.
#[cfg(windows)]
pub(super) fn snapshot() -> Result<Vec<Proc>, String> {
    // Every session's: a daemon started at the desk runs in the desk's session while an ssh logon runs in session 0.
    // Another account's process shows no ExecutablePath to this one, so it never matches an install's path.
    let script = "@(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine) | ConvertTo-Json -Compress";
    list(Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]), cmd::ASK_BOUND, parse_cim)
}

/// CIM's JSON: an array of objects, or one object for a single process, each with a numeric `ProcessId` and
/// `ParentProcessId`; `None` when it is anything else (`{}`, `null`, a string), which would read as processes that match
/// nothing.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows reads its processes from CIM"))]
pub(super) fn parse_cim(text: &str) -> Option<Vec<Proc>> {
    let v = serde_json::from_str::<serde_json::Value>(text.trim()).ok()?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    let s = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |o: &serde_json::Value, k: &str| o.get(k).and_then(serde_json::Value::as_u64).and_then(|x| u32::try_from(x).ok());
    items
        .iter()
        .map(|o| Some(Proc { pid: n(o, "ProcessId")?, ppid: n(o, "ParentProcessId")?, name: s(o, "Name"), exe: s(o, "ExecutablePath"), args: s(o, "CommandLine") }))
        .collect()
}

/// Whether a process runs the program `bin` of the install at `prefix` (`sot`, `sotd`). Unix: named so by the kernel,
/// with argv[0] exactly that path. Windows: the image path, or for the daemon the image an apply renamed aside
/// (`bin\sotd-stale-*.exe`), compared without regard to case.
pub(super) fn runs(p: &Proc, prefix: &Path, bin: &str) -> bool {
    if cfg!(windows) {
        let dir = prefix.join("bin");
        let exe = p.exe.to_ascii_lowercase();
        let want = dir.join(format!("{bin}.exe")).to_string_lossy().to_ascii_lowercase();
        let stale = dir.join(format!("{bin}-stale-")).to_string_lossy().to_ascii_lowercase();
        return !exe.is_empty() && (exe == want || (bin == "sotd" && exe.starts_with(&stale) && exe.ends_with(".exe")));
    }
    p.name == bin && p.exe == prefix.join("bin").join(bin).to_string_lossy()
}

/// Whether a daemon process of the install may run: it does when the listing shows one, and it is presumed to when the
/// listing failed, since absence is never concluded from a listing that could not be read.
pub(super) fn daemon_may_run(all: &Result<Vec<Proc>, String>, prefix: &Path) -> bool {
    match all {
        Ok(all) => !daemon_pids(all, prefix).is_empty(),
        Err(_) => true,
    }
}

/// The pids of the install's daemon processes.
pub(super) fn daemon_pids(all: &[Proc], prefix: &Path) -> Vec<u32> {
    all.iter().filter(|p| runs(p, prefix, "sotd")).map(|p| p.pid).collect()
}

/// Whether a window of the install at `prefix` runs.
pub(super) fn window_runs(all: &[Proc], prefix: &Path) -> bool {
    all.iter().any(|p| runs(p, prefix, "sot"))
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod tests;
