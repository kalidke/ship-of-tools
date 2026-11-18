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
        ("no-entry", Shown::default()),
        ("done", shown("done", "2026-01-01T00:00:02Z")),
    ];
    let order: Vec<&str> = strip_order(rows, |(_, s)| s.clone()).into_iter().map(|(name, _)| name).collect();
    assert_eq!(order, ["blocked", "done", "working-new", "working-old", "idle", "no-entry"]);
}
