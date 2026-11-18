//! The boot resume's pace and order (BOOT-STARTS-EVERY-ROW-AT-ONCE).

use super::*;
use crate::rows::work_state::Shown;
use std::sync::Mutex;

/// A row whose start spawns begins at least `BOOT_START_SPACING` after the last one that did, in the order given; a row
/// whose supervisor answers begins at once and takes no turn. Before this rule a boot began 29 rows within 5 s.
#[tokio::test(start_paused = true)]
async fn boot_starts_that_spawn_begin_one_per_spacing_and_a_row_that_answers_waits_for_none() {
    let t0 = tokio::time::Instant::now();
    let begun = Arc::new(Mutex::new(Vec::new()));
    let rows = vec![("a", true), ("b", false), ("c", true), ("d", true), ("e", false), ("f", true)];
    let seen = begun.clone();
    let joins = pace_starts(rows, BOOT_START_SPACING, |row| std::future::ready(row.1), |row| {
        seen.lock().unwrap().push((row.0, t0.elapsed()));
        tokio::spawn(async {})
    })
    .await;
    for j in joins {
        j.await.unwrap();
    }
    let (s, zero) = (BOOT_START_SPACING, Duration::ZERO);
    assert_eq!(*begun.lock().unwrap(), [("a", zero), ("b", zero), ("c", s), ("d", 2 * s), ("e", 2 * s), ("f", 3 * s)]);
}

/// The pace is three seconds: the other tests take the constant as their unit, so only this one fails when it changes.
#[test]
fn the_boot_spacing_is_three_seconds() {
    assert_eq!(BOOT_START_SPACING, Duration::from_secs(3));
}

/// The rows start in the order a window's strip shows them by default: a row waiting on its person, then done, working,
/// waiting and the rest, the newest stamp first within each.
#[test]
fn rows_start_in_the_strips_default_order() {
    let shown = |state: &str, at: &str| Shown { state: state.into(), summary: String::new(), status_at: at.into() };
    let rows = vec![
        ("idle", shown("idle", "2026-01-01T00:00:09Z")),
        ("working-old", shown("working", "2026-01-01T00:00:01Z")),
        ("blocked", shown("blocked", "2026-01-01T00:00:00Z")),
        ("working-new", shown("working", "2026-01-01T00:00:05Z")),
        ("waiting", shown("waiting", "2026-01-01T00:00:08Z")),
        ("no-entry", Shown::default()),
        ("done", shown("done", "2026-01-01T00:00:02Z")),
    ];
    let order: Vec<&str> = strip_order(rows, |(_, s)| s.clone()).into_iter().map(|(name, _)| name).collect();
    assert_eq!(order, ["blocked", "done", "working-new", "working-old", "waiting", "idle", "no-entry"]);
}

/// `SOT_COMM_HOME` and `SOT_SELF_HOST` pinned to a scratch registry for one test, and restored.
struct CommEnv {
    _serial: std::sync::MutexGuard<'static, ()>,
    saved: [(&'static str, Option<std::ffi::OsString>); 2],
}

impl CommEnv {
    fn new(comm_home: &Path) -> CommEnv {
        let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = ["SOT_COMM_HOME", "SOT_SELF_HOST"].map(|key| (key, std::env::var_os(key)));
        std::env::set_var("SOT_COMM_HOME", comm_home);
        std::env::set_var("SOT_SELF_HOST", "host-t");
        CommEnv { _serial: serial, saved }
    }
}

impl Drop for CommEnv {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// A capsule row `label` with a fence file under `state` and the agent handle `handle`.
fn fenced_row(workspaces: &Workspaces, state: &Path, project: &Path, label: &str, handle: &str) {
    let mut row = crate::rows::Workspace::from_label(label, project.to_path_buf(), false, "none".into(), String::new(), String::new());
    row.workspace_id = format!("ws-pace-{label}");
    row.agent_handle = crate::rows::handle::Declared::new(handle.to_string());
    let dir = super::super::state_dir_for(state, &row.workspace_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("supervisor.lock"), b"").unwrap();
    workspaces.insert(row);
}

/// The whole resume, with the run gate closed so no row spawns: every held row reads `queued` once marked and none after
/// the resume returns, rows begin in the strip's order (the registry's work-state: blocked, then waiting, then idle, a
/// row with no entry last) and at least a spacing apart.
#[tokio::test(start_paused = true)]
async fn a_boot_resume_queues_every_row_then_begins_each_in_strip_order_a_spacing_apart() {
    let (state, project, comm) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let _env = CommEnv::new(comm.path());
    let entry = |state: &str, at: &str, handle: &str| {
        (handle.to_string(), serde_json::json!({"host": "host-t", "state": state, "status_at": at}))
    };
    let agents: serde_json::Map<_, _> =
        [entry("idle", "2026-01-01T00:00:09Z", "h-idle"), entry("waiting", "2026-01-01T00:00:01Z", "h-wait"), entry("blocked", "2026-01-01T00:00:00Z", "h-block")]
            .into_iter()
            .collect();
    std::fs::write(comm.path().join("registry.json"), serde_json::json!({"agents": agents}).to_string()).unwrap();
    let workspaces = Workspaces::new();
    for (label, handle) in [("idle", "h-idle"), ("none", "h-none"), ("wait", "h-wait"), ("block", "h-block")] {
        fenced_row(&workspaces, state.path(), project.path(), label, handle);
    }
    assert!(workspaces.close_gate_and_settle(std::time::Instant::now()));
    let spacing = Duration::from_millis(150);
    let poll = Duration::from_millis(5);
    let t0 = tokio::time::Instant::now();
    let done = std::sync::atomic::AtomicBool::new(false);
    let (seen_queued, first_unqueued) = {
        let resume = async {
            resume_paced(state.path().to_path_buf(), workspaces.clone(), spacing).await;
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        let watch = async {
            let mut queued = std::collections::BTreeSet::new();
            let mut unqueued = std::collections::BTreeMap::new();
            let mut scan = || {
                for ws in workspaces.list() {
                    if ws.shown_phase() == "queued" {
                        queued.insert(ws.slug.clone());
                    } else if queued.contains(&ws.slug) {
                        unqueued.entry(ws.slug.clone()).or_insert_with(|| t0.elapsed());
                    }
                }
            };
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                scan();
                tokio::time::sleep(poll).await;
            }
            scan();
            (queued, unqueued)
        };
        tokio::join!(resume, watch).1
    };
    assert!(workspaces.list().iter().all(|ws| ws.shown_phase() != "queued"), "none reads queued after the resume returns");
    // The first row begins before a poll can run (nothing but its probe lies between the mark and its turn), so the
    // poll sees the other three read `queued` and begin in order.
    let mut begun: Vec<_> = first_unqueued.iter().collect();
    begun.sort_by_key(|(_, at)| **at);
    let order_by_time: Vec<&str> = begun.iter().map(|(slug, _)| slug.as_str()).collect();
    assert_eq!(seen_queued.iter().map(String::as_str).collect::<Vec<_>>(), ["idle", "none", "wait"], "rows read queued once marked");
    assert_eq!(order_by_time, ["wait", "idle", "none"], "the rest begin in the strip's order: {begun:?}");
    for pair in begun.windows(2) {
        assert!(*pair[1].1 - *pair[0].1 + poll >= spacing, "begins a spacing apart: {begun:?}");
    }
}

/// The order's one read of the registry is bounded: a read that never returns leaves the rows in the order given, after
/// the bound, instead of holding every row back.
#[tokio::test]
async fn a_registry_read_that_never_returns_leaves_the_rows_in_the_order_given() {
    let (state, project, comm) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let _env = CommEnv::new(comm.path());
    let workspaces = Workspaces::new();
    let mut candidates = Vec::new();
    for label in ["b", "a", "c"] {
        fenced_row(&workspaces, state.path(), project.path(), label, "");
        let ws = workspaces.resolve(Some(&format!("ws-pace-{label}"))).unwrap();
        candidates.push((ws.workspace_id.clone(), ws.agent(), ws.project_root.clone(), ws.agent_name(), ws.slug.clone()));
    }
    let given: Vec<String> = candidates.iter().map(|c| c.0.clone()).collect();
    let (release, stalled) = std::sync::mpsc::channel::<()>();
    let bound = Duration::from_millis(100);
    let t0 = std::time::Instant::now();
    let ordered = tokio::time::timeout(
        Duration::from_secs(20),
        in_strip_order_from(&workspaces, candidates, move || {
            let _ = stalled.recv();
            None
        }, bound),
    )
    .await
    .expect("rows are not held back by a read that never returns");
    assert_eq!(ordered.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), given);
    assert!(t0.elapsed() >= bound, "{:?}", t0.elapsed());
    drop(release);
}
