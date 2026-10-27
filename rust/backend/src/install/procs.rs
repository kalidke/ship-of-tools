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

/// The bound on a process listing: a cold CIM query takes about 26 s on a hosted Windows runner (scripts/CLAUDE.md,
/// Installers: "Bounded waits").
pub(super) const LIST_BOUND: Duration = Duration::from_secs(60);

/// This account's processes, or why they could not be listed (see [`listed`]): never an empty list, since this process
/// is always one of them.
pub(super) fn snapshot() -> Result<Vec<Proc>, String> {
    #[cfg(unix)]
    return listed(&mut listing(), LIST_BOUND, parse_ps);
    #[cfg(windows)]
    return listed(&mut listing(), LIST_BOUND, parse_cim);
}

/// The listing `cmd` prints within `bound` (through the run's bounded runner, which ends what it started), read by
/// `parse`. One that was cut at its bound, did not start, was ended by a signal, exited nonzero, or printed no process
/// that reads is `Err` with its reason.
pub(super) fn listed(cmd: &mut Command, bound: Duration, parse: fn(&str) -> Vec<Proc>) -> Result<Vec<Proc>, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let ran = cmd::output_within(cmd, bound);
    match ran.code {
        Some(0) => Some(parse(&ran.out)).filter(|all| !all.is_empty()).ok_or_else(|| format!("`{program}` printed no process list")),
        Some(code) => Err(format!("`{program}` ended with exit {code}: {}", ran.last_said())),
        None if ran.err.trim().is_empty() => Err(format!("`{program}` was ended by a signal")),
        None => Err(format!("`{program}` gave no answer: {}", ran.err.trim())),
    }
}

/// The listing a final check reads: one that gave no answer is FAIL under `check` with its reason, since `unknown` (what
/// the check judges) is not known.
pub(super) fn checked(listing: Result<Vec<Proc>, String>, found: &mut super::verdict::Found, check: &str, unknown: &str) -> Option<Vec<Proc>> {
    listing.map_err(|why| found.fail(check, format!("could not list this account's processes ({why}), so {unknown} is unknown"))).ok()
}

#[cfg(unix)]
fn listing() -> Command {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut c = Command::new("ps");
    c.args(["-u", &uid.to_string(), "-ww", "-o", "pid=", "-o", "ppid=", "-o", "ucomm=", "-o", "args="]);
    c
}

/// `ps -o pid= -o ppid= -o ucomm= -o args=` lines; a line that is not one is skipped.
#[cfg_attr(windows, allow(dead_code, reason = "Unix reads its processes from ps"))]
pub(super) fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let name = words.next()?.to_string();
            let args: Vec<&str> = words.collect();
            Some(Proc { pid, ppid, name, exe: args.first().map(|s| (*s).to_string()).unwrap_or_default(), args: args.join(" ") })
        })
        .collect()
}

#[cfg(windows)]
fn listing() -> Command {
    // Every session's: a daemon started at the desk runs in the desk's session while an ssh logon runs in session 0.
    // Another account's process shows no ExecutablePath to this one, so it never matches an install's path.
    let script = "@(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine) | ConvertTo-Json -Compress";
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    c
}

/// CIM's JSON: an array of objects, or one object for a single process.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows reads its processes from CIM"))]
pub(super) fn parse_cim(text: &str) -> Vec<Proc> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) else { return Vec::new() };
    let items = match v {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };
    let s = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |o: &serde_json::Value, k: &str| o.get(k).and_then(serde_json::Value::as_u64).and_then(|x| u32::try_from(x).ok()).unwrap_or(0);
    items
        .iter()
        .map(|o| Proc { pid: n(o, "ProcessId"), ppid: n(o, "ParentProcessId"), name: s(o, "Name"), exe: s(o, "ExecutablePath"), args: s(o, "CommandLine") })
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

/// The pids of the install's daemon processes.
pub(super) fn daemon_pids(all: &[Proc], prefix: &Path) -> Vec<u32> {
    all.iter().filter(|p| runs(p, prefix, "sotd")).map(|p| p.pid).collect()
}

/// Whether a window of the install at `prefix` runs.
pub(super) fn window_runs(all: &[Proc], prefix: &Path) -> bool {
    all.iter().any(|p| runs(p, prefix, "sot"))
}
