//! The row liveness stamp: while a row runs, its handle's `last_seen` stays fresh, so an idle row is live.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::comm::registry::registry::{comm_handle_for_workspace, iso8601_utc_now, stamp_last_seen};
use crate::rows::workspace::Phase;
use crate::rows::Workspaces;

/// How often the held handles are stamped when nothing changed.
const STAMP_EVERY: Duration = Duration::from_secs(60);

/// How often the task looks at the rows.
const TICK: Duration = Duration::from_secs(2);

/// The non-empty handles of Starting or Ready rows: the sessions a daemon is running now.
pub(crate) fn held(rows: impl IntoIterator<Item = (Phase, String)>) -> BTreeSet<String> {
    rows.into_iter()
        .filter(|(phase, handle)| matches!(phase, Phase::Starting | Phase::Ready) && !handle.is_empty())
        .map(|(_, handle)| handle)
        .collect()
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
        let now_held = held(workspaces.list().iter().map(|ws| (ws.phase(), comm_handle_for_workspace(ws))));
        let since = last_pass.map_or(STAMP_EVERY, |t| t.elapsed());
        if (now_held.is_empty() && last_held.is_empty()) || !due(&now_held, &last_held, since) {
            continue;
        }
        let (handles, stamp) = (now_held.clone(), iso8601_utc_now());
        match tokio::task::spawn_blocking(move || stamp_last_seen(&handles, &stamp)).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!("row liveness stamp skipped: the comm registry could not be written; retrying"),
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
        let rows = [
            (Phase::Ready, "a".to_string()),
            (Phase::Starting, "b".to_string()),
            (Phase::Stopped, "c".to_string()),
            (Phase::Terminal, "d".to_string()),
            (Phase::Ready, String::new()),
        ];
        assert_eq!(held(rows), ["a".to_string(), "b".to_string()].into_iter().collect());
    }

    #[test]
    fn a_pass_is_due_for_a_changed_set_or_a_minute() {
        let a: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let b: BTreeSet<String> = ["b".to_string()].into_iter().collect();
        assert!(due(&a, &b, Duration::from_secs(1)));
        assert!(due(&a, &a, STAMP_EVERY));
        assert!(!due(&a, &a, STAMP_EVERY - Duration::from_secs(1)));
    }
}
