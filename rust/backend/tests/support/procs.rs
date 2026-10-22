//! Process, cgroup-scope and user-service helpers the `Env` fixture and tests share.

use super::*;

/// The pids of every `sot-capsule` process this test build's own executable started over `state_root`, each with
/// whether it is the supervisor (`supervise`), listed from `/proc`: a process whose argv is `<exe or its file name>
/// <subcommand> <a path under state_root>` (argv[0] is the full path when `systemd-run` execs the supervisor, a scoped
/// launch, and only the file name when the durable parent's native launcher does). Supervisors first, then any other
/// process. `Err`, with the account, whenever `/proc` could not be read: a listing that fails must never read as "no
/// capsule is running". Never panics, so [`Env`]'s `Drop` can call it; [`own_capsule_pids`] is its panicking form.
#[cfg(target_os = "linux")]
fn try_own_capsule_procs(state_root: &Path) -> Result<Vec<(bool, u32)>, String> {
    use std::os::unix::ffi::OsStrExt;
    let exe = sot_capsule_exe();
    let name = exe.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    let entries = std::fs::read_dir("/proc").map_err(|error| format!("read /proc for the capsule process list: {error}"))?;
    let mut found: Vec<(bool, u32)> = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        // A process that ended between the listing and the read is not one to kill.
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let mut argv = cmdline.split(|b| *b == 0).map(|a| std::ffi::OsStr::from_bytes(a));
        let (Some(argv0), Some(sub), Some(root)) = (argv.next(), argv.next(), argv.next()) else {
            continue;
        };
        if (argv0 == exe.as_os_str() || argv0 == name.as_os_str()) && Path::new(root).starts_with(state_root) {
            found.push((sub != "supervise", pid));
        }
    }
    found.sort();
    Ok(found)
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

/// Whether `pid` is gone: no `/proc/<pid>`, or a zombie (dead, only unreaped), which holds no command line and which a
/// listing by argv never shows.
#[cfg(target_os = "linux")]
fn pid_is_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        // The state letter follows the last `)` of the command name.
        Ok(stat) => stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

/// Polls every 50 ms until `pid` is gone; `false` when it still lives after `timeout`.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn poll_until_pid_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pid_is_gone(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// SIGKILL each of `pids` by number, then poll each gone within `BOUND`. A pid is the test's own spawn by the listing
/// it came from ([`own_capsule_pids`]), never by a pattern.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn kill_pids_and_wait(pids: &[u32]) {
    for pid in pids {
        unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
    }
    for pid in pids {
        assert!(poll_until_pid_gone(*pid, BOUND), "capsule process {pid} still lives after SIGKILL");
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

/// SIGKILL this env's own supervisor by pid ([`own_supervisor_pids`]), then poll it gone: simulates the authority
/// crashing outright (never a graceful `stop`, which would publish its own end-of-authority state cleanly).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn kill_supervisor(state_root: &Path) {
    kill_pids_and_wait(&own_supervisor_pids(state_root));
}

/// SIGKILL every process this env's capsule has left by pid, the supervisor first and gone before the rest so it
/// starts nothing after, then poll each gone: the row's whole capsule crashing.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn kill_capsule(state_root: &Path) {
    kill_supervisor(state_root);
    kill_pids_and_wait(&own_capsule_pids(state_root));
}
