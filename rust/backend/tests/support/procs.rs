//! Process, cgroup-scope and user-service helpers the `Env` fixture and tests share.

use super::*;

/// The reads a capsule listing makes of `/proc`, behind a trait so a test can make each of them answer or fail
/// deterministically.
#[cfg(target_os = "linux")]
pub trait ProcFs {
    /// Every pid `/proc` lists; an entry that cannot be read is an `Err`, never skipped.
    fn pids(&self) -> std::io::Result<Vec<u32>>;
    /// `/proc/<pid>/cmdline`, NUL-separated.
    fn cmdline(&self, pid: u32) -> std::io::Result<Vec<u8>>;
}

/// The real `/proc`.
#[cfg(target_os = "linux")]
pub struct RealProcFs;

#[cfg(target_os = "linux")]
impl ProcFs for RealProcFs {
    fn pids(&self) -> std::io::Result<Vec<u32>> {
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            if let Some(pid) = entry?.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                pids.push(pid);
            }
        }
        Ok(pids)
    }

    fn cmdline(&self, pid: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(format!("/proc/{pid}/cmdline"))
    }
}

/// Whether a read failed because the process is gone (it ended between the listing and the read), the one failure a
/// listing treats as the process's absence. Any other failure (permission, I/O) is not evidence of absence.
#[cfg(target_os = "linux")]
fn is_gone_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Whether `cmdline` is a capsule of this test build over `state_root`: `Some(true)` for a supervisor, `Some(false)`
/// for any other subcommand, `None` when it is not one of ours. The command line is `<exe or its file name>
/// <subcommand> <a path under state_root>` (argv[0] is the full path when `systemd-run` execs the supervisor, a scoped
/// launch, and only the file name when the durable parent's native launcher does).
#[cfg(target_os = "linux")]
fn capsule_kind(cmdline: &[u8], exe: &Path, state_root: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let name = exe.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    let mut argv = cmdline.split(|b| *b == 0).map(std::ffi::OsStr::from_bytes);
    let (argv0, sub, root) = (argv.next()?, argv.next()?, argv.next()?);
    ((argv0 == exe.as_os_str() || argv0 == name.as_os_str()) && Path::new(root).starts_with(state_root))
        .then_some(sub == "supervise")
}

/// The pids of every `sot-capsule` process of `exe` over `state_root`, each with whether it is the supervisor,
/// supervisors first. `Err`, with the account, whenever a read failed for any reason but the process being gone: a
/// listing that fails must never read as "no capsule is running".
#[cfg(target_os = "linux")]
pub fn select_capsule_procs(fs: &dyn ProcFs, exe: &Path, state_root: &Path) -> Result<Vec<(bool, u32)>, String> {
    let mut found: Vec<(bool, u32)> = Vec::new();
    for pid in fs.pids().map_err(|error| format!("list /proc for the capsule process list: {error}"))? {
        match fs.cmdline(pid) {
            Ok(cmdline) => {
                if let Some(supervisor) = capsule_kind(&cmdline, exe, state_root) {
                    found.push((!supervisor, pid));
                }
            }
            Err(error) if is_gone_error(&error) => {}
            Err(error) => return Err(format!("read /proc/{pid}/cmdline for the capsule process list: {error}")),
        }
    }
    found.sort();
    Ok(found)
}

/// [`select_capsule_procs`] over the real `/proc` and this build's own `sot-capsule`. Never panics, so [`Env`]'s
/// `Drop` can call it; [`own_capsule_pids`] is its panicking form.
#[cfg(target_os = "linux")]
fn try_own_capsule_procs(state_root: &Path) -> Result<Vec<(bool, u32)>, String> {
    select_capsule_procs(&RealProcFs, &sot_capsule_exe(), state_root)
}

/// Windows: the pids of every `sot-capsule.exe` this test build's own
/// executable started over `state_root`, each with whether it is the supervisor,
/// supervisors first, then any other subcommand. One PowerShell call over stdin (no
/// command-line quoting to get wrong); a path is embedded as a
/// single-quoted literal with its quotes doubled, and matched with
/// `.ToLower().Contains(...)`, never `-like`, since a path may hold `[`.
/// `Err`, with PowerShell's account, whenever it could not answer (it did not start, did not end within `BOUND`, or
/// exited non-zero): a listing that fails must never read as "no capsule is running". Never panics, so [`Env`]'s `Drop`
/// can call it; [`own_capsule_pids`] is its panicking form. The script ends with an
/// empty line: `-Command -` runs a multi-line statement read from stdin
/// only once an empty line follows it.
#[cfg(windows)]
fn try_own_capsule_procs(state_root: &Path) -> Result<Vec<(bool, u32)>, String> {
    use std::io::Write;
    // A path as given AND as the filesystem names it: a TEMP in 8.3 short
    // form (`RUNNER~1`) reaches a capsule's command line in long form, and
    // `canonicalize` gives that long form (its `\\?\` prefix stripped).
    fn lits(p: &Path) -> String {
        let mut forms = vec![p.to_string_lossy().into_owned()];
        if let Ok(c) = std::fs::canonicalize(p) {
            let c = c.to_string_lossy().into_owned();
            let c = c.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(c);
            if !forms.contains(&c) {
                forms.push(c);
            }
        }
        let quoted: Vec<String> = forms.iter().map(|f| format!("'{}'", f.replace('\'', "''").to_lowercase())).collect();
        format!("@({})", quoted.join(", "))
    }
    let script = format!(
        "$exes = {exe}; $roots = {root};\n\
         foreach ($p in @(Get-CimInstance Win32_Process -Filter \"Name='sot-capsule.exe'\")) {{\n\
           if ($p.ExecutablePath -and $p.CommandLine) {{\n\
             $cl = $p.CommandLine.ToLower();\n\
             $inRoot = $false;\n\
             foreach ($r in $roots) {{ if ($cl.Contains($r)) {{ $inRoot = $true }} }}\n\
             if (($exes -contains $p.ExecutablePath.ToLower()) -and $inRoot) {{\n\
               $kind = 'other';\n\
               if ($cl.Contains(' supervise ')) {{ $kind = 'supervise' }}\n\
               Write-Output ('{{0}} {{1}}' -f $p.ProcessId, $kind)\n\
             }}\n\
           }}\n\
         }}\n\
         \n",
        exe = lits(&sot_capsule_exe()),
        root = lits(state_root),
    );
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start powershell for the capsule process list: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    // The script is the child's input, so this waits through `drain` rather than `run_within`.
    let (status, stdout, stderr) = sot_platform::test_isolated::drain(child)
        .try_wait_within(BOUND)
        .map_err(|failure| format!("the capsule process list: {failure}"))?;
    if !status.success() {
        return Err(format!("the capsule process list failed: {status}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"));
    }
    let mut found: Vec<(bool, u32)> = stdout
        .lines()
        .filter_map(|l| {
            let (pid, kind) = l.trim().split_once(' ')?;
            Some((kind != "supervise", pid.parse().ok()?))
        })
        .collect();
    found.sort();
    Ok(found)
}

/// The pids of every `sot-capsule` process this test build started over `state_root`, supervisors first: see
/// [`try_own_capsule_procs`]. `Err`, with the account, whenever the listing could not be read.
#[cfg(any(windows, target_os = "linux"))]
pub fn try_own_capsule_pids(state_root: &Path) -> Result<Vec<u32>, String> {
    Ok(try_own_capsule_procs(state_root)?.into_iter().map(|(_, pid)| pid).collect())
}

/// [`try_own_capsule_pids`], failing the caller with its reason when the listing cannot be read.
#[cfg(any(windows, target_os = "linux"))]
#[allow(dead_code)]
pub fn own_capsule_pids(state_root: &Path) -> Vec<u32> {
    try_own_capsule_pids(state_root).unwrap_or_else(|error| panic!("{error}"))
}

/// The supervisors among [`own_capsule_pids`]: "no second supervisor over a live one" counts these.
#[cfg(any(windows, target_os = "linux"))]
#[allow(dead_code)]
pub fn own_supervisor_pids(state_root: &Path) -> Vec<u32> {
    try_own_capsule_procs(state_root)
        .unwrap_or_else(|error| panic!("{error}"))
        .into_iter()
        .filter_map(|(other, pid)| (!other).then_some(pid))
        .collect()
}

/// `PATH` with every directory removed that contains a file named any of
/// `names`, so a test that needs an agent to be ABSENT does not depend on
/// what the box it runs on happens to have installed.
#[allow(dead_code)]
pub fn path_without(names: &[&str]) -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| !names.iter().any(|n| dir.join(n).is_file()))
        .collect();
    std::env::join_paths(kept).expect("rejoin scrubbed PATH")
}

/// How many supervisors [`own_supervisor_pids`] lists, `Err` with the listing's account when it could not answer: a
/// sampler's query failure must fail the test, never count as "zero processes". Never panics.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn try_own_supervisor_count(state_root: &Path) -> Result<usize, String> {
    Ok(try_own_capsule_procs(state_root)?.iter().filter(|(other, _)| !other).count())
}

/// Polls every 50 ms until [`own_capsule_pids`] lists nothing; the pids still listed when `timeout` passes (empty: all
/// gone).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn poll_until_no_capsule_pids(state_root: &Path, timeout: Duration) -> Vec<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = own_capsule_pids(state_root);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A4b: the cgroup2 path of `pid`, the trimmed `0::` line of
/// `/proc/<pid>/cgroup`.
#[cfg(target_os = "linux")]
pub fn cgroup_rel(pid: u32) -> String {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_else(|e| panic!("read /proc/{pid}/cgroup: {e}"));
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|rel| rel.trim().to_string())
        .unwrap_or_else(|| panic!("no 0:: line in /proc/{pid}/cgroup: {text:?}"))
}

/// A4b: kills a row scope this test created when the test ends, panic or
/// not. Only [`arm_scope_guard`] builds one, once it has seen the scope.
#[cfg(target_os = "linux")]
pub struct ScopeKillGuard {
    rel: String,
    hash: String,
}

#[cfg(target_os = "linux")]
impl ScopeKillGuard {
    /// A4b: polls every 100 ms until the scope's `cgroup.events` is gone or
    /// reads `populated 0`; on timeout panics with that file and `cgroup.procs`.
    /// Arming saw the scope under the same root, so a missing file means the
    /// scope was removed.
    pub async fn assert_empties(&self, within: Duration) {
        let rel = &self.rel;
        let dir = row_scope_aim::v2_root().join(rel.trim_start_matches('/'));
        let deadline = Instant::now() + within;
        loop {
            let events = match std::fs::read_to_string(dir.join("cgroup.events")) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Ok(text) if text.lines().any(|l| l.trim() == "populated 0") => return,
                other => other,
            };
            if Instant::now() >= deadline {
                let procs = std::fs::read_to_string(dir.join("cgroup.procs"));
                panic!("scope {rel} did not empty within {within:?}: cgroup.events {events:?}, cgroup.procs {procs:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ScopeKillGuard {
    fn drop(&mut self) {
        // The aim rule runs again just before the write: the scope's identity
        // is the one kept at arming, the caller's cgroup is read now.
        let own = cgroup_rel(std::process::id());
        if let Err(e) = row_scope_aim::aim(&self.rel, &own, &self.hash) {
            eprintln!("ScopeKillGuard: the aim rule refuses {} at drop, nothing written: {e}", self.rel);
            return;
        }
        let kill = row_scope_aim::v2_root().join(self.rel.trim_start_matches('/')).join("cgroup.kill");
        if kill.exists() {
            let _ = std::fs::write(&kill, "1");
        }
    }
}

/// A control group's end is read and written under production's cgroup v2 root (`row_scope_aim::v2_root`). Where
/// that root holds no v2 hierarchy, a test would start what it cannot see end; it refuses before any row starts.
#[cfg(target_os = "linux")]
pub fn require_cgroup_v2_root() {
    let root = row_scope_aim::v2_root();
    assert!(
        root.join("cgroup.controllers").exists(),
        "no cgroup v2 hierarchy at production's root {root:?} (row_scope_aim::v2_root: /sys/fs/cgroup, or /sys/fs/cgroup/unified on a hybrid host): this host cannot show a control group's end"
    );
}

/// A4b: a guard on the scope `rel` under production's cgroup v2 root
/// (`row_scope_aim::v2_root`), armed only when PRODUCTION's aim rule
/// accepts `rel` for this test's own row ([`aim_scope_guard`]) and that
/// root shows the scope. Panics otherwise, so a capture bug or a reused pid
/// can never aim the guard at a live session, and
/// [`ScopeKillGuard::assert_empties`] never reads an unseen scope as emptied.
#[cfg(target_os = "linux")]
pub fn arm_scope_guard(rel: &str, state_dir: &Path) -> ScopeKillGuard {
    let hash = aim_scope_guard(rel, &cgroup_rel(std::process::id()), state_dir);
    let events = row_scope_aim::v2_root().join(rel.trim_start_matches('/')).join("cgroup.events");
    assert!(
        events.exists(),
        "the scope is not at {events:?}, under production's cgroup v2 root (row_scope_aim::v2_root): a test cannot judge a scope it cannot see"
    );
    ScopeKillGuard { rel: rel.to_string(), hash }
}

/// [`arm_scope_guard`]'s aim check with the caller's own cgroup given, for the
/// aim table: `state_dir`'s hash when production's aim rule accepts `rel` (the
/// prefix carries that hash, and `rel` is not `own_rel` or an ancestor of it);
/// panics otherwise.
#[cfg(target_os = "linux")]
pub fn aim_scope_guard(rel: &str, own_rel: &str, state_dir: &Path) -> String {
    let hash = sot_platform::host::state_dir::state_dir_hash(state_dir);
    if let Err(e) = row_scope_aim::aim(rel, own_rel, &hash) {
        panic!("arm_scope_guard: the production aim rule refuses this target: {e}");
    }
    hash
}

/// ADR 0043 decision 32, test 1's own SKIP gate: does THIS test runner
/// have a `systemd --user` manager reachable at all? Same capability
/// question `rows::spawn::detach::user_scope_available` answers in
/// production, duplicated here rather than exposed from `sot-backend`
/// (that function is private to its own crate) — the two probes are a
/// handful of lines each and answer the same question for genuinely
/// different callers, not worth a shared crate-boundary-crossing export.
/// `Err` is the probe's whole account (its command, exit status and both
/// streams), or why it has none, within production's own 5 s probe
/// bound — exactly what the caller prints on `SKIPPED:`.
#[cfg(target_os = "linux")]
pub fn user_manager_available_for_test() -> Result<(), String> {
    let ran = sot_platform::test_wait::try_run_within(
        Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--", "/bin/true"]),
        Duration::from_secs(5),
    )?;
    if ran.status.success() {
        Ok(())
    } else {
        Err(ran.to_string())
    }
}
/// Stop the scratch unit [`Env::spawn_sotd_as_user_service`] started —
/// NEVER the live `sotd`'s own unit, a distinct `sot-test-<uuid>.service`
/// this test alone owns. Two proofs, both required (Codex BLOCKER 1,
/// reproduced with a failing `systemctl` stub: the old version's ignored
/// `status()` plus an `is-active` check that folded a command ERROR to
/// `unwrap_or(false)` — "not active" — let a stop that never actually ran
/// read as success): (1) `systemctl --user stop` itself reports success —
/// a command error, a nonzero exit, ANY failure here is a hard test
/// failure, never silently treated as "done"; (2) `daemon_pid` — read
/// back by the caller from `MainPID` right after spawn, never re-derived
/// here — has actually exited (`/proc/<pid>` gone), polled rather than
/// trusted the instant `stop` returns. `is-active` alone is not enough
/// for (2): it can still read `"deactivating"` mid-shutdown, which would
/// let the caller's sustained-survival window start before the daemon
/// backing this unit is actually dead.
#[cfg(target_os = "linux")]
pub fn stop_user_service(unit: &str, daemon_pid: u32) {
    sot_platform::test_wait::run_within(Command::new("systemctl").args(["--user", "stop", unit]), BOUND).ok();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !Path::new(&format!("/proc/{daemon_pid}")).exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "systemctl --user stop {unit} reported success but pid {daemon_pid} is still alive after 10s"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// SIGKILL each of `pids`, each through a pidfd opened BEFORE its identity is read again: from the open until the
/// descriptor closes the pid names that one process, so a pid the kernel has reused for another process is never
/// signaled. A process whose command line no longer names a capsule of `exe` over `state_root` is not ours and is left
/// alone; one that is gone counts as gone. Then waits, bounded, for each signaled process to end. `Err` for any failure
/// other than the process being gone.
#[cfg(target_os = "linux")]
pub fn kill_verified(fs: &dyn ProcFs, pids: &[u32], exe: &Path, state_root: &Path) -> Result<(), String> {
    use sot_platform::identity::challenge_unix::{pidfd_open, poll_pidfd_readable};
    use std::os::fd::AsRawFd;
    let mut held = Vec::new();
    for &pid in pids {
        let fd = match pidfd_open(pid) {
            Ok(fd) => fd,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(error) => return Err(format!("pidfd_open({pid}): {error}")),
        };
        match fs.cmdline(pid) {
            Ok(cmdline) if capsule_kind(&cmdline, exe, state_root).is_some() => {}
            Ok(_) => continue,
            Err(error) if is_gone_error(&error) => continue,
            Err(error) => return Err(format!("read /proc/{pid}/cmdline after opening its pidfd: {error}")),
        }
        // SAFETY: pidfd_send_signal takes a pidfd, a signal number, a null siginfo and flags 0.
        let rc = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), libc::SIGKILL, std::ptr::null::<libc::c_void>(), 0) };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(format!("pidfd_send_signal({pid}): {error}"));
        }
        held.push((pid, fd));
    }
    for (pid, fd) in held {
        match poll_pidfd_readable(fd.as_raw_fd(), BOUND) {
            Ok(true) => {}
            Ok(false) => return Err(format!("capsule process {pid} still lives {BOUND:?} after SIGKILL")),
            Err(error) => return Err(format!("wait for capsule process {pid} to end: {error}")),
        }
    }
    Ok(())
}

/// [`kill_verified`] over this build's own `sot-capsule` and the real `/proc`, failing the test on any `Err`.
#[cfg(target_os = "linux")]
fn kill_own(pids: &[u32], state_root: &Path) {
    kill_verified(&RealProcFs, pids, &sot_capsule_exe(), state_root).unwrap_or_else(|error| panic!("{error}"));
}

/// SIGKILL this env's own supervisor ([`own_supervisor_pids`], through [`kill_verified`]) and wait it gone: simulates
/// the authority crashing outright (never a graceful `stop`, which would publish its own end-of-authority state
/// cleanly). A test that kills a supervisor expects one: finding none fails it.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn kill_supervisor(state_root: &Path) {
    let supervisors = own_supervisor_pids(state_root);
    assert!(!supervisors.is_empty(), "no supervisor of {state_root:?} to kill");
    kill_own(&supervisors, state_root);
}

/// SIGKILL every process this env's capsule has: the supervisor first and gone before the rest is listed again, so it
/// starts nothing after: the row's whole capsule crashing. A capsule that has no process fails the test.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn kill_capsule(state_root: &Path) {
    assert!(!own_capsule_pids(state_root).is_empty(), "no capsule process of {state_root:?} to kill");
    let supervisors = own_supervisor_pids(state_root);
    if !supervisors.is_empty() {
        kill_own(&supervisors, state_root);
    }
    kill_own(&own_capsule_pids(state_root), state_root);
}

/// Windows: end each of `pids` by a process handle opened first, then re-list: while the handle is open the pid names
/// that one process, so a pid still listed afterward is the capsule the listing found and one not listed is left
/// alone. Waits, bounded, for each to end. `Err` for any failure other than the process being gone (`OpenProcess`
/// answers 87, invalid parameter, for a pid that no longer exists).
#[cfg(windows)]
pub fn kill_by_handle(state_root: &Path, pids: &[u32]) -> Result<(), String> {
    use sot_platform::identity::challenge_win::{terminate_handle, wait_handle};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
    let mut held: Vec<(u32, HANDLE)> = Vec::new();
    let close = |held: &[(u32, HANDLE)]| {
        for (_, handle) in held {
            unsafe { CloseHandle(*handle) };
        }
    };
    for &pid in pids {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(87) {
                continue;
            }
            close(&held);
            return Err(format!("OpenProcess({pid}): {error}"));
        }
        held.push((pid, handle));
    }
    let result = (|| {
        let still = try_own_capsule_pids(state_root)?;
        for (pid, handle) in &held {
            if still.contains(pid) {
                terminate_handle(*handle).map_err(|error| format!("TerminateProcess({pid}): {error}"))?;
            }
        }
        for (pid, handle) in &held {
            if still.contains(pid) && !wait_handle(*handle, BOUND).map_err(|error| format!("wait for {pid}: {error}"))? {
                return Err(format!("capsule process {pid} still lives {BOUND:?} after TerminateProcess"));
            }
        }
        Ok(())
    })();
    close(&held);
    result
}
