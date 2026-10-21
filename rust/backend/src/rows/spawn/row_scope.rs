//! A row's own systemd scope: capture, remembered list, and the aimed kill at destroy.

use crate::rows::spawn::row_scope_aim::{aim, prefix, v2_root};
use sot_platform::host::state_dir::state_dir_hash;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The cgroup fence's own `QUIESCENCE_TIMEOUT` (`sot_log::claude`).
pub(crate) const SCOPE_EMPTY_BOUND: Duration = Duration::from_secs(10);
/// The row's remembered scopes, one cgroup rel per line, in its state
/// dir. Written at capture, before any stop or kill, and removed only
/// once an end proves every listed scope empty or gone, so neither a
/// retry nor a restarted daemon counts the row ended while a scope it
/// captured may still hold processes.
pub(crate) const SCOPES_FILE: &str = "row-scopes";

#[cfg(test)]
thread_local! {
    /// A unit test's fake cgroup root; never `/sys/fs/cgroup`.
    pub(crate) static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The cgroup v2 root every end aims at ([`v2_root`]).
pub(crate) fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return root;
    }
    v2_root()
}

/// `systemd-run --unit` value for a new scoped supervisor of this row.
pub(crate) fn unit_name(state_dir: &Path) -> String {
    format!("{}{}.scope", prefix(&state_dir_hash(state_dir)), uuid::Uuid::now_v7().simple())
}

/// This process's own cgroup2 path, the `0::` line of `/proc/self/cgroup`.
pub(crate) fn own_rel() -> Option<String> {
    rel_of(&std::fs::read_to_string("/proc/self/cgroup").ok()?)
}

fn rel_of(proc_cgroup: &str) -> Option<String> {
    proc_cgroup.lines().find_map(|l| l.strip_prefix("0::")).map(|rel| rel.trim().to_string())
}

/// The scope of the supervisor `pid`, captured before the end and
/// listed in [`SCOPES_FILE`] before anything is stopped or killed.
pub(crate) fn capture(root: &Path, state_dir: &Path, pid: u32) -> Result<Option<String>, String> {
    match std::fs::read_to_string(format!("/proc/{pid}/cgroup")) {
        Ok(text) => capture_from(root, state_dir, &text),
        Err(_) => Ok(None),
    }
}

/// The `0::` path of `proc_cgroup` when its leaf is a scope of this
/// row and `cgroup.kill` is there to end it, listed durably in
/// [`SCOPES_FILE`]; `Ok(None)` ends the row exactly as before this
/// module (an unscoped or older row, a frontend-spawned drawer, or a scope
/// with no `cgroup.kill` under `root`, for the cause [`no_kill_cause`]
/// names). `Err` is a failed write: nothing is killed.
pub(crate) fn capture_from(root: &Path, state_dir: &Path, proc_cgroup: &str) -> Result<Option<String>, String> {
    let Some(rel) = rel_of(proc_cgroup) else { return Ok(None) };
    let leaf = rel.rsplit('/').next().unwrap_or("");
    if !(leaf.starts_with(&prefix(&state_dir_hash(state_dir))) && leaf.ends_with(".scope")) {
        return Ok(None);
    }
    let scope = at(root, &rel);
    let kill = scope.join("cgroup.kill");
    if let Err(e) = std::fs::metadata(&kill) {
        if e.kind() == ErrorKind::NotFound {
            tracing::warn!(
                path = %kill.display(),
                "capsule workspace: no cgroup.kill for the row's scope ({}); this scope is not ended through \
                 its cgroup",
                no_kill_cause(root, &scope)
            );
            return Ok(None);
        }
    }
    remember(state_dir, std::slice::from_ref(&rel))?;
    Ok(Some(rel))
}

/// Why `scope`, named by a supervisor's `0::` path, has no `cgroup.kill`
/// under `root`, checked in this order: no cgroup v2 hierarchy at `root`;
/// no such scope under it (removed since `/proc` named it, or a path from
/// another cgroup namespace); else a kernel without `cgroup.kill`.
fn no_kill_cause(root: &Path, scope: &Path) -> &'static str {
    if !root.join("cgroup.controllers").exists() {
        "no cgroup v2 hierarchy at this root"
    } else if !scope.exists() {
        "the scope is not under this root: removed already, or named from another cgroup namespace"
    } else {
        "Linux before 5.14"
    }
}

/// The scopes [`SCOPES_FILE`] lists; an absent file lists none.
pub(crate) fn listed(state_dir: &Path) -> Result<Vec<String>, String> {
    let path = state_dir.join(SCOPES_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("read {path:?}: {e}")),
    }
}

/// Adds `rels` to [`SCOPES_FILE`], durably.
fn remember(state_dir: &Path, rels: &[String]) -> Result<(), String> {
    let mut all = listed(state_dir)?;
    let before = all.len();
    for rel in rels {
        if !all.contains(rel) {
            all.push(rel.clone());
        }
    }
    if all.len() == before {
        return Ok(());
    }
    rewrite(state_dir, &all)
}

/// [`SCOPES_FILE`] lists exactly `rels`, durably; none removes it.
fn rewrite(state_dir: &Path, rels: &[String]) -> Result<(), String> {
    let path = state_dir.join(SCOPES_FILE);
    let written = if rels.is_empty() {
        crate::durable::remove(&path)
    } else {
        crate::durable::write(&path, format!("{}\n", rels.join("\n")).as_bytes())
    };
    written.map_err(|e| format!("record the row's scopes in {path:?}: {e}"))
}

/// Kill every scope [`SCOPES_FILE`] lists, plus `captured` and every
/// sibling scope of this row beside it (an adopted leg's older scope),
/// then wait up to `bound` for each to be empty or gone.
pub(crate) fn end(
    root: &Path,
    own_rel: &str,
    state_dir: &Path,
    captured: Option<&str>,
    bound: Duration,
) -> Result<(), String> {
    let hash = state_dir_hash(state_dir);
    let mut scopes = listed(state_dir)?;
    if let Some(rel) = captured {
        let parent = &rel[..rel.rfind('/').unwrap_or(0)];
        let dir = at(root, parent);
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("list {dir:?}: {e}"))?;
        for entry in entries {
            let name = entry.map_err(|e| format!("list {dir:?}: {e}"))?.file_name();
            let name = name.to_string_lossy();
            let scope = format!("{parent}/{name}");
            if name.starts_with(&prefix(&hash)) && name.ends_with(".scope") && !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
    }
    if scopes.is_empty() {
        return Ok(());
    }
    end_set(root, own_rel, state_dir, &hash, scopes, bound)
}

fn end_set(
    root: &Path,
    own_rel: &str,
    state_dir: &Path,
    hash: &str,
    scopes: Vec<String>,
    bound: Duration,
) -> Result<(), String> {
    remember(state_dir, &scopes)?;
    for scope in &scopes {
        aim(scope, own_rel, hash)?;
        match std::fs::write(at(root, scope).join("cgroup.kill"), "1") {
            Ok(()) => {}
            Err(e) if gone(&e) => {}
            Err(e) => return Err(format!("kill {scope}: {e}")),
        }
    }
    let deadline = Instant::now() + bound;
    loop {
        let mut left = Vec::new();
        for scope in &scopes {
            if populated(root, scope)? {
                left.push(scope.clone());
            }
        }
        if left.is_empty() {
            return rewrite(state_dir, &[]);
        }
        if Instant::now() >= deadline {
            rewrite(state_dir, &left)?;
            return Err(format!("the row's scope did not empty within {bound:?}: {left:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Only an absent `cgroup.events` (the scope is gone) or a line
/// `populated 0` is empty; any other read error is an `Err`.
fn populated(root: &Path, rel: &str) -> Result<bool, String> {
    match std::fs::read_to_string(at(root, rel).join("cgroup.events")) {
        Ok(text) => Ok(!text.lines().any(|l| l.trim() == "populated 0")),
        Err(e) if gone(&e) => Ok(false),
        Err(e) => Err(format!("read {rel}/cgroup.events: {e}")),
    }
}

/// NotFound or `ENODEV`: the cgroup was removed.
fn gone(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENODEV)
}

fn at(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.trim_start_matches('/'))
}

#[cfg(test)]
#[path = "row_scope_tests.rs"]
mod tests;
