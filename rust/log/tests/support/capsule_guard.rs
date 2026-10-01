//! A spawned `sot-capsule` that no panicking test can leave behind.
//!
//! A plain `std::process::Child` is not killed when an assert panics before
//! the test's own kill, and a `--survival normal` `run` leg outlives its
//! supervisor by design, so killing the supervise child is not enough: the
//! guard also sweeps every `supervise` and `run` process anchored on the
//! test's OWN state root (its tempdir path), the same sweep the backend's
//! test `Env::drop` does. Included by each test file with
//! `#[path = "support/capsule_guard.rs"] mod capsule_guard;`.

use std::path::{Path, PathBuf};
use std::process::Child;

pub struct CapsuleGuard {
    child: Option<Child>,
    exe: PathBuf,
    state_root: PathBuf,
}

/// Whether `root` is safe to build a kill pattern from: absolute, no `..`,
/// and STRICTLY below the temp dir. An empty or unanchored pattern would
/// match every process, a production daemon included.
pub fn sweep_root_ok(root: &Path) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    sweep_root_ok_in(root, &std::env::temp_dir(), home.as_deref(), xdg.as_deref())
}

/// [`sweep_root_ok`] with the environment passed in. Also refuses a temp dir
/// with no normal component (TMPDIR=`/`) and any root at or under the
/// production state dirs.
pub fn sweep_root_ok_in(root: &Path, tmp: &Path, home: Option<&Path>, xdg: Option<&Path>) -> bool {
    use std::path::Component;
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return false;
    }
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    if !tmp.components().any(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let mut protected = vec![PathBuf::from("/run/user")];
    if let Some(home) = home {
        protected.push(home.join(".local/share/sot"));
        protected.push(home.join(".sot-comm"));
    }
    if let Some(xdg) = xdg {
        protected.push(xdg.to_path_buf());
    }
    if protected.iter().any(|p| root.starts_with(p)) {
        return false;
    }
    root.starts_with(tmp) && root != tmp
}

impl CapsuleGuard {
    /// `state_root` is the exact path the process was given on its argv.
    /// Panics when `sweep_root_ok` is false, so no guard with a bad root
    /// can exist.
    pub fn new(child: Child, state_root: impl Into<PathBuf>) -> Self {
        Self::new_for_exe(child, env!("CARGO_BIN_EXE_sot-capsule"), state_root)
    }

    /// As [`new`], for a process launched from a copy of the binary.
    pub fn new_for_exe(child: Child, exe: impl Into<PathBuf>, state_root: impl Into<PathBuf>) -> Self {
        let state_root = state_root.into();
        assert!(
            sweep_root_ok(&state_root),
            "CapsuleGuard refuses root {state_root:?}: it must be absolute, free of `..`, and strictly below the temp dir"
        );
        Self { child: Some(child), exe: exe.into(), state_root }
    }

    #[allow(dead_code)]
    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("capsule child still held").id()
    }

    #[allow(dead_code)]
    pub fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("capsule child still held")
    }
}

impl Drop for CapsuleGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            // Bounded: a capsule stuck in uninterruptible sleep must not hang the binary.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !matches!(c.try_wait(), Ok(Some(_)) | Err(_)) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        // Never panic here: a panic during unwinding aborts the process.
        if !sweep_root_ok(&self.state_root) {
            return;
        }
        #[cfg(target_os = "linux")]
        {
            use std::process::{Command, Stdio};
            use std::time::{Duration, Instant};
            // Supervisors first each pass, so no new leg is spawned after
            // this pass's supervisor-kill lands; repeat until a pass finds
            // nothing or 2 s pass.
            // The supervise child carries the exe as spawned, its legs the
            // canonical one: sweep both spellings.
            let mut exes = vec![self.exe.clone()];
            if let Ok(canon) = std::fs::canonicalize(&self.exe) {
                if canon != self.exe {
                    exes.push(canon);
                }
            }
            let sweeps: Vec<String> = exes
                .iter()
                .flat_map(|e| {
                    ["supervise", "run"].map(|sub| build_leg_pgrep_pattern(e, sub, &self.state_root))
                })
                .collect();
            let combined: Vec<String> = exes
                .iter()
                .map(|e| build_leg_pgrep_pattern(e, "(supervise|run)", &self.state_root))
                .collect();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                for pattern in &sweeps {
                    let _ = Command::new("pkill")
                        .arg("-9")
                        .arg("-f")
                        .arg(pattern)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                if !combined.iter().any(|p| any_process_matches(p)) || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Regex-escape a path for safe use inside an `-f` pattern (`pkill`/`pgrep`
/// use POSIX extended regex).
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
/// `^<escaped exe path> <subcommand> <escaped state_root>(/| |$)`; the tail
/// keeps `/tmp/.tmpABC` from matching `/tmp/.tmpABCD`.
#[cfg(target_os = "linux")]
pub fn build_leg_pgrep_pattern(exe: &Path, subcommand: &str, state_root: &Path) -> String {
    format!("^{} {subcommand} {}(/| |$)", regex_escape_path(exe), regex_escape_path(state_root))
}

/// Whether any live process's command line matches `pattern`.
#[cfg(target_os = "linux")]
pub fn any_process_matches(pattern: &str) -> bool {
    use std::process::{Command, Stdio};
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
