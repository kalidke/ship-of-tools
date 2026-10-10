//! A row's own systemd scope: capture, remembered list, and the aimed kill at destroy.

use crate::rows::spawn::row_scope_aim::{aim, prefix};
use sot_log::host::state_dir::state_dir_hash;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const CGROUP_ROOT: &str = "/sys/fs/cgroup";
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

/// The cgroup2 root every end aims at.
pub(crate) fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return root;
    }
    PathBuf::from(CGROUP_ROOT)
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
/// module (an unscoped or older row, a frontend-spawned drawer, a
/// kernel before 5.14). `Err` is a failed write: nothing is killed.
pub(crate) fn capture_from(root: &Path, state_dir: &Path, proc_cgroup: &str) -> Result<Option<String>, String> {
    let Some(rel) = rel_of(proc_cgroup) else { return Ok(None) };
    let leaf = rel.rsplit('/').next().unwrap_or("");
    if !(leaf.starts_with(&prefix(&state_dir_hash(state_dir))) && leaf.ends_with(".scope")) {
        return Ok(None);
    }
    if let Err(e) = std::fs::metadata(at(root, &rel).join("cgroup.kill")) {
        if e.kind() == ErrorKind::NotFound {
            tracing::warn!(
                scope = %rel,
                "capsule workspace: no cgroup.kill (Linux before 5.14); a child that left the agent's \
                 process group survives this row's end"
            );
            return Ok(None);
        }
    }
    remember(state_dir, std::slice::from_ref(&rel))?;
    Ok(Some(rel))
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
mod tests {
    use super::*;

    /// `root{rel}` as a fake cgroup: an empty `cgroup.kill` and the
    /// given `cgroup.events`. Never under `/sys/fs/cgroup`.
    fn fake_scope(root: &Path, rel: &str, events: &str) -> PathBuf {
        let dir = root.join(rel.trim_start_matches('/'));
        std::fs::create_dir_all(&dir).expect("mkdir fake scope");
        std::fs::write(dir.join("cgroup.kill"), "").expect("write cgroup.kill");
        std::fs::write(dir.join("cgroup.events"), events).expect("write cgroup.events");
        dir
    }

    const OWN: &str = "/a/app.slice/run-u1.scope";

    #[test]
    fn scope_aim_refuses_everything_but_this_rows_scope() {
        let state = tempfile::tempdir().expect("tempdir");
        let h = state_dir_hash(state.path());
        for (target, own, accepted) in crate::rows::spawn::row_scope_aim::aim_table(&h) {
            let verdict = aim(&target, &own, &h);
            assert_eq!(
                verdict.is_ok(),
                accepted,
                "aim {} {target:?} with own {own:?}: {verdict:?}",
                if accepted { "refused" } else { "accepted" }
            );
        }
    }

    #[test]
    fn the_v2_root_is_sys_fs_cgroup_when_it_is_the_v2_mount_else_its_unified_child() {
        use crate::rows::spawn::row_scope_aim::v2_root_in;
        let base = tempfile::tempdir().unwrap();
        assert_eq!(v2_root_in(base.path()), base.path().join("unified"), "a hybrid host");
        std::fs::write(base.path().join("cgroup.controllers"), "").unwrap();
        assert_eq!(v2_root_in(base.path()), base.path(), "a unified host");
    }

    #[test]
    fn capture_finds_only_this_rows_scope() {
        let (state, other_state, root) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (h, other) = (state_dir_hash(state.path()), state_dir_hash(other_state.path()));
        let ours = format!("/a/app.slice/sot-row-{h}-x.scope");
        fake_scope(root.path(), &ours, "populated 1\n");
        let theirs = format!("/a/app.slice/sot-row-{other}-x.scope");
        fake_scope(root.path(), &theirs, "populated 1\n");
        let no_kill = format!("/a/app.slice/sot-row-{h}-y.scope");
        std::fs::create_dir_all(root.path().join(no_kill.trim_start_matches('/'))).unwrap();
        let at = |text: String| capture_from(root.path(), state.path(), &text).expect("capture");

        assert_eq!(at(format!("0::{ours}\n")), Some(ours.clone()), "this row's scope");
        assert_eq!(at("0::/a/app.slice/run-u5.scope\n".to_string()), None, "a run-u scope");
        assert_eq!(at(format!("0::{theirs}\n")), None, "another row's scope");
        assert_eq!(at(format!("12:pids:{ours}\n1:name=systemd:{ours}\n")), None, "v1 lines only");
        assert_eq!(at(format!("0::{no_kill}\n")), None, "no cgroup.kill");
    }

    #[test]
    fn scope_end_kills_every_scope_of_this_row_and_nothing_else() {
        let (state, other_state, root) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (h, other) = (state_dir_hash(state.path()), state_dir_hash(other_state.path()));
        let a = fake_scope(root.path(), &format!("/a/app.slice/sot-row-{h}-a.scope"), "populated 0\nfrozen 0\n");
        let b = fake_scope(root.path(), &format!("/a/app.slice/sot-row-{h}-b.scope"), "populated 0\nfrozen 0\n");
        let c = fake_scope(root.path(), &format!("/a/app.slice/sot-row-{other}-c.scope"), "populated 0\nfrozen 0\n");

        let rel = format!("/a/app.slice/sot-row-{h}-a.scope");
        end(root.path(), OWN, state.path(), Some(&rel), Duration::from_millis(200)).expect("end");
        let kill = |d: &Path| std::fs::read_to_string(d.join("cgroup.kill")).unwrap();
        assert_eq!(kill(&a), "1", "the captured scope was not killed");
        assert_eq!(kill(&b), "1", "a sibling scope of this row was not killed");
        assert_eq!(kill(&c), "", "another row's scope was killed");
    }

    #[test]
    fn scope_that_does_not_empty_keeps_the_row_not_ended() {
        let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let h = state_dir_hash(state.path());
        let rel = format!("/a/app.slice/sot-row-{h}-a.scope");
        let dir = fake_scope(root.path(), &rel, "populated 1\nfrozen 0\n");
        let bound = Duration::from_millis(100);

        let err = end(root.path(), OWN, state.path(), Some(&rel), bound).expect_err("end on a populated scope");
        assert!(err.contains("did not empty"), "{err}");
        end(root.path(), OWN, state.path(), None, bound).expect_err("a retry on a still-populated scope");
        std::fs::write(dir.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
        end(root.path(), OWN, state.path(), None, bound).expect("a retry once emptied");
        end(root.path(), OWN, state.path(), None, bound).expect("a second retry");

        let fresh = tempfile::tempdir().unwrap();
        let rel = format!("/a/app.slice/sot-row-{}-a.scope", state_dir_hash(fresh.path()));
        let dir = fake_scope(root.path(), &rel, "populated 1\n");
        end(root.path(), OWN, fresh.path(), Some(&rel), bound).expect_err("end on a populated scope");
        std::fs::remove_dir_all(&dir).unwrap();
        end(root.path(), OWN, fresh.path(), None, bound).expect("a scope that is gone is empty");
    }

    #[test]
    fn scope_events_unreadable_is_not_empty() {
        let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let rel = format!("/a/app.slice/sot-row-{}-a.scope", state_dir_hash(state.path()));
        let dir = root.path().join(rel.trim_start_matches('/'));
        std::fs::create_dir_all(dir.join("cgroup.events")).unwrap();
        std::fs::write(dir.join("cgroup.kill"), "").unwrap();
        end(root.path(), OWN, state.path(), Some(&rel), Duration::from_millis(100))
            .expect_err("an unreadable cgroup.events is not an empty scope");
    }

    /// A row whose authority and leg are both proven absent, so
    /// `end_run`'s unreachable arm reaches its `Unheld` answer.
    fn absent_row(state_dir: &Path) {
        let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
        sot_log::supervisor::journal::pointer::publish(state_dir, voyage_id).expect("publish the pointer");
        let voyage_root = sot_log::supervisor::voyage_root_path(state_dir, voyage_id);
        std::fs::create_dir_all(&voyage_root).expect("voyage root");
        std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
    }

    /// Points `root()` at a fake cgroup root for this test's thread.
    struct FakeRoot;
    impl FakeRoot {
        fn at(root: &Path) -> Self {
            TEST_ROOT.with(|r| *r.borrow_mut() = Some(root.to_path_buf()));
            FakeRoot
        }
    }
    impl Drop for FakeRoot {
        fn drop(&mut self) {
            TEST_ROOT.with(|r| *r.borrow_mut() = None);
        }
    }

    #[test]
    fn row_scope_file_written_at_capture_before_any_kill() {
        let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let _root = FakeRoot::at(root.path());
        let rel = format!("/a/app.slice/sot-row-{}-a.scope", state_dir_hash(state.path()));
        let dir = fake_scope(root.path(), &rel, "populated 0\n");
        let kill = || std::fs::read_to_string(dir.join("cgroup.kill")).unwrap();

        // Capture lists the scope on disk and kills nothing.
        let text = format!("0::{rel}\n");
        assert_eq!(capture_from(root.path(), state.path(), &text), Ok(Some(rel.clone())));
        assert_eq!(listed(state.path()), Ok(vec![rel.clone()]), "the file after capture");
        assert_eq!(kill(), "", "capture killed");
        // A capture-then-return arm (the transport error, Failed,
        // Refused, OutcomeUnknown, Starting) returns here; the file
        // stays, and the next capture adds nothing twice.
        assert_eq!(capture_from(root.path(), state.path(), &text), Ok(Some(rel.clone())));
        assert_eq!(listed(state.path()), Ok(vec![rel.clone()]), "the file after a second capture");

        // The supervisor-outcome arms: the file lists the scope when
        // the stop runs, and the kill comes only after it.
        let mut at_stop = None;
        crate::rows::run::end_run::stop_then_end_scope(state.path(), Some(&rel), root.path(), &crate::rows::spawn::row_scope::own_rel().unwrap_or_default(), || {
            at_stop = Some((listed(state.path()), kill()))
        })
        .expect("end an empty scope");
        assert_eq!(at_stop, Some((Ok(vec![rel.clone()]), String::new())), "the file and kill at the stop");
        assert_eq!(kill(), "1", "the scope was not killed after the stop");
        assert!(!state.path().join(SCOPES_FILE).exists(), "a proven-empty end left the file");
    }

    #[test]
    fn every_not_ended_scope_path_keeps_the_file() {
        let bound = Duration::from_millis(100);
        for fault in ["aim refusal", "kill write error", "events read error", "parent-dir list error"] {
            let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let rel = format!("/a/app.slice/sot-row-{}-a.scope", state_dir_hash(state.path()));
            let dir = root.path().join(rel.trim_start_matches('/'));
            let mut own = OWN.to_string();
            match fault {
                "aim refusal" => {
                    fake_scope(root.path(), &rel, "populated 1\n");
                    own = rel.clone();
                }
                "kill write error" => {
                    std::fs::create_dir_all(dir.join("cgroup.kill")).unwrap();
                    std::fs::write(dir.join("cgroup.events"), "populated 0\n").unwrap();
                }
                "events read error" => {
                    std::fs::create_dir_all(dir.join("cgroup.events")).unwrap();
                    std::fs::write(dir.join("cgroup.kill"), "").unwrap();
                }
                _ => {
                    std::fs::create_dir_all(root.path().join("a")).unwrap();
                    std::fs::write(root.path().join("a/app.slice"), "").unwrap();
                }
            }
            let captured = capture_from(root.path(), state.path(), &format!("0::{rel}\n"));
            assert_eq!(captured, Ok(Some(rel.clone())), "{fault}: capture");
            end(root.path(), &own, state.path(), Some(&rel), bound).expect_err(fault);
            assert_eq!(listed(state.path()), Ok(vec![rel.clone()]), "{fault}: the file after the end");
            end(root.path(), &own, state.path(), None, bound).expect_err(&format!("{fault}: a retry ended"));
            assert_eq!(listed(state.path()), Ok(vec![rel.clone()]), "{fault}: the file after the retry");
        }
    }

    #[test]
    fn scope_file_left_by_a_killed_daemon_is_ended_by_the_next_end() {
        use crate::rows::run::end_run::EndRunOutcome;
        let root = tempfile::tempdir().unwrap();
        let _root = FakeRoot::at(root.path());
        for (events, ends) in [("populated 0\n", true), ("populated 1\n", false)] {
            let state = tempfile::tempdir().unwrap();
            absent_row(state.path());
            let rel = format!("/a/app.slice/sot-row-{}-a.scope", state_dir_hash(state.path()));
            let dir = fake_scope(root.path(), &rel, events);
            // Only the file: no in-process state names this scope.
            std::fs::write(state.path().join(SCOPES_FILE), format!("{rel}\n")).unwrap();

            let outcome = crate::rows::run::end_run::end_run(state.path(), "test reason", true);
            assert_eq!(std::fs::read_to_string(dir.join("cgroup.kill")).unwrap(), "1", "{events}: not killed");
            if ends {
                assert!(matches!(outcome, Ok(EndRunOutcome::Unheld)), "{events}: {outcome:?}");
                assert!(!state.path().join(SCOPES_FILE).exists(), "{events}: the file stayed");
            } else {
                assert!(matches!(outcome, Ok(EndRunOutcome::NotEnded(_))), "{events}: {outcome:?}");
                assert_eq!(listed(state.path()), Ok(vec![rel]), "{events}: the file after NotEnded");
            }
        }
    }
}
