//! The comm wake (0031 B3): the daemon's one way of telling a row it has mail.
//!
//! Every [`TICK`] each capsule row with a declared handle is checked. If its
//! `inbox/<h>.jsonl` holds lines past `read/<h>.cursor` that are new since the
//! last wake, and the row sits at a free prompt, the row is typed
//! [`WAKE_LINE`] and Enter. A row the daemon cannot type into (not a capsule,
//! not Ready, attach fails, no wake predicate for its agent) counts as no row:
//! it is skipped and tried again next tick; its end-of-turn check still reads
//! the mail. This module only READS the inbox and cursor.
//!
//! "Last woken" lives in the tick task's memory, never on disk, so a daemon
//! restart wakes every row with unread mail once, at its first free prompt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::capsule_workspace::headless::{wake_if_free, WakeOutcome};
use crate::workspaces::Workspaces;

pub const TICK: Duration = Duration::from_secs(2);
/// Mail still unread this long after a wake gets one more line.
const REPEAT_AFTER: Duration = Duration::from_secs(600);
const WAKE_LINE: &[u8] = b"[sot-comm] you have mail: run comm-poll.sh";
const CONTROLLER_ID: &str = "sot-comm-wake";
const OP_BUDGET: Duration = Duration::from_secs(3);
const QUIET_BUDGET: Duration = Duration::from_millis(300);
const PACING_BUDGET: Duration = Duration::from_secs(1);

/// When a handle was last woken: the inbox's complete-line count then.
#[derive(Debug, Clone, Copy)]
struct Woken {
    line: u64,
    at: Instant,
}

/// One look at `inbox/<h>.jsonl` against `read/<h>.cursor`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// Complete lines in the inbox.
    total: u64,
    /// Counted lines past the cursor.
    unread: u64,
    /// Counted lines past the cursor AND past the last wake.
    fresh: u64,
}

/// Opens the inbox fresh (a cached size can lag another box's append on a
/// shared home), reads up to the last `\n` only, and counts the lines at
/// index >= max(cursor, `woken_line`) that are to `handle` and not from it.
fn scan(comm_home: &Path, handle: &str, woken_line: u64) -> Scan {
    let cursor = std::fs::read_to_string(comm_home.join("read").join(format!("{handle}.cursor")))
        .ok()
        .and_then(|c| c.split_whitespace().next().and_then(|f| f.parse::<u64>().ok()))
        .unwrap_or(0);
    let Ok(bytes) = std::fs::read(comm_home.join("inbox").join(format!("{handle}.jsonl"))) else {
        return Scan::default();
    };
    let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Scan::default();
    };
    let mut out = Scan::default();
    for (i, line) in bytes[..end].split(|b| *b == b'\n').enumerate() {
        let i = i as u64;
        out.total = i + 1;
        if i < cursor || !counts(line, handle) {
            continue;
        }
        out.unread += 1;
        if i >= woken_line {
            out.fresh += 1;
        }
    }
    out
}

fn counts(line: &[u8], handle: &str) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return false;
    };
    v.get("to").and_then(|t| t.as_str()) == Some(handle)
        && v.get("from").and_then(|f| f.as_str()) != Some(handle)
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Nothing unread: forget the handle.
    Clear,
    /// Unread, but already woken for it.
    Hold,
    Wake,
}

fn decide(s: &Scan, woken: Option<&Woken>, now: Instant) -> Decision {
    if s.unread == 0 {
        Decision::Clear
    } else if s.fresh > 0 || woken.map_or(true, |w| now.duration_since(w.at) >= REPEAT_AFTER) {
        Decision::Wake
    } else {
        Decision::Hold
    }
}

/// The prompt glyph of an agent the daemon can read, `None` for one it cannot.
/// Codex ships OFF: no Codex screen has been captured, and a glyph is never
/// guessed, so a Codex row counts as a row the daemon cannot type into.
fn prompt_glyph(agent: &str) -> Option<char> {
    match agent {
        "claude" => Some('\u{276f}'),
        _ => None,
    }
}

/// The free-prompt test: the cursor sits on the empty input line. The row
/// indexes a real line, the line holds the glyph with only spaces before it,
/// and the cursor column is just after the glyph (or one more, over a space,
/// no-break space, tab or nothing). A dialog, a draft or a working session is
/// not free.
pub(crate) fn prompt_free(lines: &[String], cursor: Option<(u16, u16)>, agent: &str) -> bool {
    let (Some(glyph), Some((row, col))) = (prompt_glyph(agent), cursor) else {
        return false;
    };
    let Some(line) = lines.get(row as usize) else {
        return false;
    };
    let cells: Vec<char> = line.chars().collect();
    let Some(g) = cells.iter().position(|c| *c == glyph) else {
        return false;
    };
    let col = col as usize;
    cells[..g].iter().all(|c| *c == ' ')
        && (col == g + 1 || (col == g + 2 && matches!(cells.get(g + 1), None | Some(' ' | '\u{a0}' | '\t'))))
}

/// The tick. Runs forever; started once from `server::run`.
pub async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let mut woken: HashMap<String, Woken> = HashMap::new();
    let mut warned: HashSet<String> = HashSet::new();
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let rows = workspaces.list();
        let mut declared: HashMap<String, usize> = HashMap::new();
        for ws in &rows {
            let h = ws.agent_handle();
            if !h.is_empty() {
                *declared.entry(h).or_default() += 1;
            }
        }
        for ws in rows {
            let handle = ws.agent_handle();
            if handle.is_empty() || ws.runtime != "capsule" {
                continue;
            }
            // Two rows declaring one handle: a wake aimed by a guess types
            // into someone else's session, so both are skipped.
            if declared.get(&handle).copied().unwrap_or(0) > 1 {
                if warned.insert(handle.clone()) {
                    tracing::warn!(handle = %handle, "comm wake: two rows declare this handle; waking neither");
                }
                continue;
            }
            let agent = ws.agent();
            if prompt_glyph(&agent).is_none() {
                continue;
            }
            let state_dir = crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
            let home = comm_home.clone();
            let prior = woken.get(&handle).copied();
            let h = handle.clone();
            let result = tokio::task::spawn_blocking(move || check_row(&home, &h, &state_dir, &agent, prior)).await;
            match result {
                Ok(Step::Clear) => {
                    woken.remove(&handle);
                }
                Ok(Step::Woke(w)) => {
                    woken.insert(handle, w);
                }
                Ok(Step::Skip) | Err(_) => {}
            }
        }
    }
}

enum Step {
    Clear,
    Skip,
    Woke(Woken),
}

fn check_row(home: &Path, handle: &str, state_dir: &Path, agent: &str, prior: Option<Woken>) -> Step {
    let s = scan(home, handle, prior.map_or(0, |w| w.line));
    match decide(&s, prior.as_ref(), Instant::now()) {
        Decision::Clear => return Step::Clear,
        Decision::Hold => return Step::Skip,
        Decision::Wake => {}
    }
    // The read-only phase: a wake check must never restart a dead row.
    let ready = crate::capsule_workspace::phase_str(sot_log::wire::SupervisorPhase::Ready);
    if crate::capsule_workspace::phase_of(state_dir) != ready {
        return Step::Skip;
    }
    match wake_if_free(
        state_dir,
        CONTROLLER_ID,
        WAKE_LINE,
        prompt_free,
        agent,
        OP_BUDGET,
        QUIET_BUDGET,
        PACING_BUDGET,
    ) {
        Ok(WakeOutcome::Woke { enter_sent: true }) => Step::Woke(Woken { line: s.total, at: Instant::now() }),
        Ok(_) => Step::Skip,
        Err(e) => {
            tracing::debug!(handle, phase = e.phase, detail = %e.detail, "comm wake: row not typeable this tick");
            Step::Skip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn free(l: &[&str], cur: Option<(u16, u16)>) -> bool {
        prompt_free(&lines(l), cur, "claude")
    }

    #[test]
    fn free_prompts() {
        assert!(free(&["banner", "\u{276f}"], Some((1, 2))));
        assert!(free(&["\u{276f}"], Some((0, 1))));
        // Ghost-text suggestion with the cursor at its start.
        assert!(free(&["\u{276f} try this"], Some((0, 1))));
        assert!(free(&["\u{276f}\u{a0}"], Some((0, 2))));
    }

    #[test]
    fn drafts_dialogs_and_missing_cursors_are_not_free() {
        assert!(!free(&["\u{276f} hello"], Some((0, 8))));
        assert!(!free(&["\u{276f}h"], Some((0, 2))));
        assert!(!free(&["\u{276f} h"], Some((0, 3))));
        assert!(!free(&["\u{276f}", "Allow this action? (y/n)"], Some((1, 24))));
        assert!(!free(&["\u{276f}"], None));
        assert!(!free(&["\u{276f}"], Some((5, 1))));
        assert!(!free(&["x \u{276f}"], Some((0, 3))));
    }

    #[test]
    fn an_agent_without_a_predicate_is_never_free() {
        assert!(!prompt_free(&lines(&["\u{203a}"]), Some((0, 1)), "codex"));
        assert!(!prompt_free(&lines(&["\u{276f}"]), Some((0, 1)), "codex"));
    }

    fn home(inbox: &str, cursor: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("inbox")).unwrap();
        std::fs::create_dir_all(d.path().join("read")).unwrap();
        std::fs::write(d.path().join("inbox/a.jsonl"), inbox).unwrap();
        if let Some(c) = cursor {
            std::fs::write(d.path().join("read/a.cursor"), c).unwrap();
        }
        d
    }

    const MINE: &str = "{\"from\":\"b\",\"to\":\"a\",\"msg\":\"x\"}\n";

    #[test]
    fn scan_counts_past_the_cursor() {
        let d = home(&MINE.repeat(3), Some("1 deadbeef\n"));
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 3, unread: 2, fresh: 2 });
        // A wake at line 2 leaves one fresh line.
        assert_eq!(scan(d.path(), "a", 2), Scan { total: 3, unread: 2, fresh: 1 });
    }

    #[test]
    fn a_cursor_that_is_not_a_number_reads_as_zero() {
        let d = home(&MINE.repeat(2), Some("oops\n"));
        assert_eq!(scan(d.path(), "a", 0).unread, 2);
        let d = home(&MINE.repeat(2), None);
        assert_eq!(scan(d.path(), "a", 0).unread, 2);
    }

    #[test]
    fn an_unterminated_tail_is_not_counted() {
        let d = home(&format!("{MINE}{}", MINE.trim_end()), None);
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 1, unread: 1, fresh: 1 });
    }

    #[test]
    fn own_and_foreign_lines_are_not_counted() {
        let inbox = format!(
            "{MINE}{{\"from\":\"a\",\"to\":\"a\",\"msg\":\"me\"}}\n{{\"from\":\"b\",\"to\":\"c\",\"msg\":\"no\"}}\nnot json\n"
        );
        let d = home(&inbox, None);
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 4, unread: 1, fresh: 1 });
    }

    #[test]
    fn batches() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(601);
        let w = Woken { line: 3, at: t0 };
        let mail = |unread, fresh| Scan { total: 3, unread, fresh };
        // After a restart the map is empty: unread mail is a new batch.
        assert_eq!(decide(&mail(3, 3), None, t0), Decision::Wake);
        // One batch, one wake: nothing fresh, not yet ten minutes.
        assert_eq!(decide(&mail(3, 0), Some(&w), t0), Decision::Hold);
        // A new line after the wake is a new batch.
        assert_eq!(decide(&mail(4, 1), Some(&w), t0), Decision::Wake);
        // Still unread ten minutes on: one repeat.
        assert_eq!(decide(&mail(3, 0), Some(&w), later), Decision::Wake);
        // Read: forgotten.
        assert_eq!(decide(&mail(0, 0), Some(&w), later), Decision::Clear);
    }
}
