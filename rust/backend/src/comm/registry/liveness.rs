//! The row liveness stamp: while a row runs, its handle's `last_seen` stays fresh, so an idle row is live.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::comm::registry::registry::{comm_handle_for_workspace, iso8601_utc_now, stamp_last_seen};
use crate::rows::workspace::Phase;
use crate::rows::{Workspace, Workspaces};

/// How often the held handles are stamped when nothing changed.
const STAMP_EVERY: Duration = Duration::from_secs(60);

/// How often the task looks at the rows.
const TICK: Duration = Duration::from_secs(2);

/// The non-empty handles of Starting or Ready rows: the sessions a daemon is running now. A row's
/// handle is resolved (a registry read) only once its phase says it runs.
pub(crate) fn held<F: FnOnce() -> String>(rows: impl IntoIterator<Item = (Phase, F)>) -> BTreeSet<String> {
    rows.into_iter()
        .filter(|(phase, _)| matches!(phase, Phase::Starting | Phase::Ready))
        .map(|(_, resolve)| resolve())
        .filter(|handle| !handle.is_empty())
        .collect()
}

/// The handles this daemon's running rows hold: each Starting or Ready row's handle by
/// `comm_handle_for_workspace`, given the daemon's whole row list, so a row never answers to a handle
/// it reaches by a self-file or stored name that another row declares (ADR 0049, one row per handle).
pub(crate) fn held_handles(rows: &[Arc<Workspace>]) -> BTreeSet<String> {
    held(rows.iter().map(|ws| (ws.phase(), move || comm_handle_for_workspace(ws, rows))))
}

/// A pass is due when the held set changed or `STAMP_EVERY` has passed since the last one.
fn due(held: &BTreeSet<String>, last_held: &BTreeSet<String>, since_last_pass: Duration) -> bool {
    held != last_held || since_last_pass >= STAMP_EVERY
}

/// Every `TICK`, stamp `last_seen` for the handles of the rows this daemon runs, when a pass is
/// due. A failed pass is one warning and is retried at the next due pass.
pub(crate) async fn run(workspaces: Workspaces) {
    let mut last_held = BTreeSet::new();
    let mut last_pass: Option<Instant> = None;
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let rows = workspaces.list();
        let Ok(now_held) = tokio::task::spawn_blocking(move || {
            held_handles(&rows)
        })
        .await
        else {
            tracing::warn!("row liveness handles could not be resolved; retrying");
            continue;
        };
        let since = last_pass.map_or(STAMP_EVERY, |t| t.elapsed());
        if (now_held.is_empty() && last_held.is_empty()) || !due(&now_held, &last_held, since) {
            continue;
        }
        let (handles, stamp) = (now_held.clone(), iso8601_utc_now());
        let rows = handles.clone();
        match tokio::task::spawn_blocking(move || stamp_last_seen(&handles, &stamp)).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                ?rows,
                "row liveness stamp failed: the comm registry could not be locked, read or written; once a row's last stamp is ten minutes old, sends to it fail as no live session"
            ),
            Err(e) => tracing::warn!("row liveness stamp task failed: {e}"),
        }
        last_pass = Some(Instant::now());
        last_held = now_held;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_is_the_handles_of_starting_and_ready_rows() {
        let rows: [(Phase, fn() -> String); 5] = [
            (Phase::Ready, || "a".to_string()),
            (Phase::Starting, || "b".to_string()),
            (Phase::Stopped, || "c".to_string()),
            (Phase::Terminal, || "d".to_string()),
            (Phase::Ready, String::new),
        ];
        assert_eq!(held(rows), ["a".to_string(), "b".to_string()].into_iter().collect());
    }

    #[test]
    fn held_resolves_a_handle_only_for_a_running_row() {
        let rows: [(Phase, Box<dyn FnOnce() -> String>); 2] =
            [(Phase::Ready, Box::new(|| "a".to_string())), (Phase::Stopped, Box::new(|| panic!("resolved a stopped row")))];
        assert_eq!(held(rows), ["a".to_string()].into_iter().collect());
    }

    #[test]
    fn a_pass_is_due_for_a_changed_set_or_a_minute() {
        let a: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let b: BTreeSet<String> = ["b".to_string()].into_iter().collect();
        assert!(due(&a, &b, Duration::from_secs(1)));
        assert!(due(&a, &a, STAMP_EVERY));
        assert!(!due(&a, &a, STAMP_EVERY - Duration::from_secs(1)));
    }

    // ADR 0049: a running row whose self-file names a handle another row
    // declares does not hold it; with no declaring row the fallback binds.
    #[test]
    fn held_handles_skips_a_handle_another_row_declares() {
        use crate::rows::workspace::{Observation, SupervisorIdentity};
        // Restores both vars when dropped, so a failed assertion leaks nothing.
        struct EnvGuard {
            _serial: std::sync::MutexGuard<'static, ()>,
            saved: [(&'static str, Option<std::ffi::OsString>); 2],
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                for (k, v) in &self.saved {
                    match v {
                        Some(v) => std::env::set_var(k, v),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
        let _guard = EnvGuard {
            saved: [("SOT_COMM_HOME", std::env::var_os("SOT_COMM_HOME")), ("SOT_SELF_HOST", std::env::var_os("SOT_SELF_HOST"))],
            _serial: crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
        };
        let dir = std::env::temp_dir().join(format!(
            "sot-filer-running-row-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(dir.join("self")).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "host-4");

        let row = |label: &str, declared: &str, ready: bool| {
            let mut w = Workspace::from_label(
                label,
                std::env::temp_dir(),
                false,
                "none".into(),
                String::new(),
                String::new(),
            );
            w.runtime = "capsule".to_string();
            w.agent_handle = std::sync::Mutex::new(declared.to_string());
            if ready {
                assert!(w.apply_phase_observation(Observation::Phase {
                    phase: Phase::Ready,
                    supervisor: SupervisorIdentity { pid: 1, created: 1 },
                    voyage: Some(uuid::Uuid::from_u128(1)),
                }));
            }
            Arc::new(w)
        };
        let older = row("m5-older", "", true);
        std::fs::write(
            dir.join("self").join(format!("host-4__{}.txt", older.workspace_id)),
            "m5-shared\nrepo=m5\nroot=/p/x\n",
        )
        .unwrap();
        let newer = row("m5-newer", "m5-shared", false);
        let both = [older.clone(), newer];
        assert!(!held_handles(&both).contains("m5-shared"));
        assert!(held_handles(&[older.clone()]).contains("m5-shared"), "no declaring row: the fallback still binds");
        let newer_ready = row("m5-newer-ready", "m5-shared", true);
        assert!(held_handles(&[older, newer_ready]).contains("m5-shared"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
