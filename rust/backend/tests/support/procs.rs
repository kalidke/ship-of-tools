//! Process, cgroup-scope and user-service helpers the `Env` fixture and tests share.

use super::*;

/// Regex-escape a path for safe use inside an `-f` pattern ([`pkill`]/
/// [`pgrep`] use POSIX extended regex) — defensive: `tempfile`'s own
/// random suffixes are plain alphanumeric today, but a path is still
/// user-influenced-shaped data, not a literal we control end to end.
#[cfg(target_os = "linux")]
fn regex_escape_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
/// The anchored `pgrep`/`pkill` pattern for a `sot-capsule` invocation:
/// `^<escaped exe path> <subcommand> <escaped state_root>`. G2 (LU4
/// review round 2): anchoring the OLD way, `^\S*sot-capsule`, silently
/// requires the character just before `sot-capsule` to be non-whitespace
/// — `\S*` cannot cross a space — so an executable path containing one
/// (a legal `CARGO_TARGET_DIR` with a space in it) never matches at all,
/// and the sweep quietly does nothing. Anchoring on the EXACT, escaped
/// executable path this suite itself resolved (`sot_capsule_exe()`) has
/// no such gap: a space in the path is not a regex metacharacter and
/// needs no escaping to match itself literally, so `regex_escape_path`
/// leaves it untouched. `subcommand` is a literal ("supervise", "run") or
/// an alternation ("(supervise|run)") — both are valid ERE on their own.
#[cfg(target_os = "linux")]
pub fn build_leg_pgrep_pattern(exe: &Path, subcommand: &str, state_root: &Path) -> String {
    format!("^{} {subcommand} {}", regex_escape_path(exe), regex_escape_path(state_root))
}
/// Windows: the pids of every `sot-capsule.exe` this test build's own
/// executable started over `state_root`, supervisors first, then `run`
/// legs, then any other subcommand. One PowerShell call over stdin (no
/// command-line quoting to get wrong); a path is embedded as a
/// single-quoted literal with its quotes doubled, and matched with
/// `.ToLower().Contains(...)`, never `-like`, since a path may hold `[`.
/// Empty when PowerShell itself cannot be run. The script ends with an
/// empty line: `-Command -` runs a multi-line statement read from stdin
/// only once an empty line follows it.
#[cfg(windows)]
pub fn own_capsule_pids(state_root: &Path) -> Vec<u32> {
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
               elseif ($cl.Contains(' run ')) {{ $kind = 'run' }}\n\
               Write-Output ('{{0}} {{1}}' -f $p.ProcessId, $kind)\n\
             }}\n\
           }}\n\
         }}\n\
         \n",
        exe = lits(&sot_capsule_exe()),
        root = lits(state_root),
    );
    let Ok(mut child) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return Vec::new();
    };
    let mut found: Vec<(u8, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, kind) = l.trim().split_once(' ')?;
            let rank = match kind {
                "supervise" => 0,
                "run" => 1,
                _ => 2,
            };
            Some((rank, pid.parse().ok()?))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, pid)| pid).collect()
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

/// Whether any live process's command line matches `pattern` — the
/// read-only half of the anchored sweep, reused by [`Env`]'s own `Drop`
/// (to poll the sweep to completion) and by the F4 cleanup-contract test
/// below (to prove both "before" and "after").
#[cfg(target_os = "linux")]
pub fn any_process_matches(pattern: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
/// G3 (LU4 review round 2): the SAME bounded, sweep-until-empty shape
/// `Env`'s own `Drop` uses for its active pkill loop, but read-only — no
/// re-signalling, since by the time a caller here needs it `Drop` has
/// already run its own loop to completion (or its own 2s bound). Exists
/// so the F4 cleanup-contract test's own "empty after" assertion is not a
/// single point-in-time check racing the exact moment `Drop`'s loop
/// itself gave up at its bound: a process that was still one syscall from
/// actually exiting when `Drop` observed its own deadline is not a real
/// leak, and re-polling here (rather than asserting instantly) is the
/// difference between a flaky false failure and a meaningful, still-
/// bounded proof.
#[cfg(target_os = "linux")]
pub fn poll_until_no_process_matches(pattern: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !any_process_matches(pattern) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
/// Drains `pipe` to EOF or `bound`, whichever comes first — mirrors
/// `capsule_workspace::runtime::drain_stderr_bounded` (production, Codex
/// SHOULD-FIX: a wrapper that has already exited can still leave stderr
/// inherited by a still-running grandchild, and a plain `read_to_string`
/// then blocks until EVERY holder of the pipe's write end closes it, not
/// just the immediate child whose own exit was already observed —
/// measured 7 s in production's own repro). Same off-thread-plus-
/// `recv_timeout` shape, duplicated rather than shared for the same
/// crate-boundary reason [`user_manager_available_for_test`]'s own doc
/// gives for duplicating the probe itself.
#[cfg(target_os = "linux")]
fn drain_stderr_bounded(mut pipe: impl std::io::Read + Send + 'static, bound: Duration) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(bound).unwrap_or_default()
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
/// not. Only [`arm_scope_guard`] builds one.
#[cfg(target_os = "linux")]
pub struct ScopeKillGuard {
    rel: String,
    hash: String,
    /// The caller's own cgroup when the arming gave one; else re-read at drop.
    own_rel: Option<String>,
}

#[cfg(target_os = "linux")]
impl Drop for ScopeKillGuard {
    fn drop(&mut self) {
        // The aim rule runs again just before the write: the scope's identity
        // is the one kept at arming, the caller's cgroup is read now.
        let own = self.own_rel.clone().unwrap_or_else(|| cgroup_rel(std::process::id()));
        if let Err(e) = row_scope_aim::aim(&self.rel, &own, &self.hash) {
            eprintln!("ScopeKillGuard: the aim rule refuses {} at drop, nothing written: {e}", self.rel);
            return;
        }
        let kill = Path::new("/sys/fs/cgroup").join(self.rel.trim_start_matches('/')).join("cgroup.kill");
        if kill.exists() {
            let _ = std::fs::write(&kill, "1");
        }
    }
}

/// A4b: a guard on `/sys/fs/cgroup{rel}`, armed only when PRODUCTION's aim
/// rule accepts `rel` for this test's own row: the prefix carries
/// `state_dir`'s hash, and `rel` is not this test process's own cgroup or
/// an ancestor of it. Panics otherwise, so a capture bug or a reused pid
/// can never aim the guard at a live session.
#[cfg(target_os = "linux")]
pub fn arm_scope_guard(rel: &str, state_dir: &Path) -> ScopeKillGuard {
    let mut guard = arm_scope_guard_against(rel, &cgroup_rel(std::process::id()), state_dir);
    guard.own_rel = None;
    guard
}

/// [`arm_scope_guard`] with the caller's own cgroup given, for the aim table.
#[cfg(target_os = "linux")]
pub fn arm_scope_guard_against(rel: &str, own_rel: &str, state_dir: &Path) -> ScopeKillGuard {
    let hash = sot_log::host::state_dir::state_dir_hash(state_dir);
    if let Err(e) = row_scope_aim::aim(rel, own_rel, &hash) {
        panic!("arm_scope_guard: the production aim rule refuses this target: {e}");
    }
    ScopeKillGuard { rel: rel.to_string(), hash, own_rel: Some(own_rel.to_string()) }
}

/// A4b: polls every 100 ms until the scope's `cgroup.events` is gone or
/// reads `populated 0`; on timeout panics with that file and `cgroup.procs`.
#[cfg(target_os = "linux")]
pub async fn assert_scope_empties(rel: &str, within: Duration) {
    let dir = Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
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

/// ADR 0043 decision 32, test 1's own SKIP gate: does THIS test runner
/// have a `systemd --user` manager reachable at all? Same capability
/// question `capsule_workspace::runtime::user_scope_available` answers in
/// production, duplicated here rather than exposed from `sot-backend`
/// (that function is private to its own crate) — the two probes are a
/// handful of lines each and answer the same question for genuinely
/// different callers, not worth a shared crate-boundary-crossing export.
/// `Err`'s message is the probe's own stderr, verbatim where there is
/// any, drained under its own separate bound — exactly what the caller
/// prints on `SKIPPED:`.
#[cfg(target_os = "linux")]
pub fn user_manager_available_for_test() -> Result<(), String> {
    let mut child = Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--", "/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("systemd-run --user --scope did not answer within 5s".to_string());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        return Ok(());
    }
    let stderr = child
        .stderr
        .take()
        .map(|pipe| drain_stderr_bounded(pipe, Duration::from_secs(1)))
        .unwrap_or_default();
    Err(stderr.trim().to_string())
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
    let status = Command::new("systemctl")
        .arg("--user")
        .arg("stop")
        .arg(unit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run systemctl --user stop");
    assert!(status.success(), "systemctl --user stop {unit} failed ({status})");

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
