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
//! A working row keeps its input box live with the cursor at the prompt, so
//! one frame cannot tell it from a row at rest. The screen is told apart by
//! holding: a row is typed into only if its screen stays identical for
//! [`STILL_FOR`] (a working row's spinner redraws several times a second).
//! The prompt glyph is `❯`, or on Windows either `❯` or `>` (Claude Code's
//! fallback when its unicode check fails, which on Windows depends on the
//! environment), and only inside the input box there. A row at rest whose
//! screen never holds still (a live clock, an animation) is never woken; this
//! fails closed and its end-of-turn check still reads the mail.
//! The converse fails open: a working row whose screen happens not to change
//! for [`STILL_FOR`] is typed into, and the line lands as a queued message.
//! Still open: no permission-menu screen has been captured. A menu at rest
//! holds still, so the hold does not protect it: if a `❯ 1. Yes` line ever
//! read free, the wake's Enter would approve a tool call.
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
/// A turn whose only change is a once-a-second counter must change inside the hold even when the screen reaches the daemon ~100 ms late.
const STILL_FOR: Duration = Duration::from_millis(1500);
/// The rule drawn above and below Claude Code's input box.
const RULE: char = '\u{2500}';

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

/// POSIX `cksum` of `bytes`: the CRC-32 (0x04C11DB7, MSB first) over the bytes
/// then the length's bytes low first, complemented, with the length.
fn cksum(bytes: &[u8]) -> (u32, u64) {
    let mut crc: u32 = 0;
    let mut feed = |b: u8| {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    };
    bytes.iter().for_each(|b| feed(*b));
    let mut n = bytes.len() as u64;
    while n != 0 {
        feed((n & 0xff) as u8);
        n >>= 8;
    }
    (!crc, bytes.len() as u64)
}

/// The cursor's line hash, `<crc>-<len>`: `comm-lib.sh`'s `_sot_hash_stdin`,
/// over the line without its newlines and NULs.
fn line_hash(line: &[u8]) -> String {
    let kept: Vec<u8> = line.iter().copied().filter(|b| *b != b'\n' && *b != 0).collect();
    let (crc, len) = cksum(&kept);
    format!("{crc}-{len}")
}

/// jq's `. > $cur` for a segment's `(fromjson? // {}) | (.ts // "")` against a
/// string. `None` where the segment emits nothing at all (a non-object JSON
/// value: `.ts` errors and jq's `//` yields no element), so it is neither
/// counted nor a boundary.
fn ts_is_greater(segment: &str, cur: &str) -> bool {
    use serde_json::Value;
    // `(fromjson? | objects) // {}`: only an object's `.ts` can be a boundary.
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(segment) else {
        return false;
    };
    match o.get("ts") {
        Some(Value::String(t)) => t.as_str() > cur,
        // null and false read as "" (never greater than a non-empty cursor);
        // true and numbers sort below every string, arrays and objects above.
        Some(Value::Array(_) | Value::Object(_)) => true,
        _ => false,
    }
}

/// How many inbox lines `read/<h>.cursor` says were shown: a port of
/// `comm-lib.sh`'s `sot_cursor_offset`, the spec, and answers what it answers
/// for every input (the cross-check test runs both).
pub(crate) fn cursor_offset(comm_home: &Path, handle: &str) -> u64 {
    let cur = std::fs::read(comm_home.join("read").join(format!("{handle}.cursor"))).unwrap_or_default();
    let cur = String::from_utf8_lossy(&cur);
    let cur = cur.trim_end_matches('\n');
    if cur.is_empty() {
        return 0;
    }
    let inbox = std::fs::read(comm_home.join("inbox").join(format!("{handle}.jsonl"))).unwrap_or_default();
    let total = inbox.iter().filter(|b| **b == b'\n').count() as u64;
    let (cnt, hash) = match cur.split_once(' ') {
        Some((c, h)) => (c, Some(h)),
        None => (cur, None),
    };
    if !cnt.is_empty() && cnt.bytes().all(|b| b.is_ascii_digit()) {
        let cnt: u64 = cnt.parse().unwrap_or(u64::MAX);
        if cnt > total {
            return if cnt == total + 1 && hash.is_some_and(|h| !h.is_empty()) { total } else { 0 };
        }
        if cnt > 0 {
            if let Some(h) = hash.filter(|h| !h.is_empty()) {
                let line = inbox.split(|b| *b == b'\n').nth(cnt as usize - 1).unwrap_or_default();
                if line_hash(line) != h {
                    return cnt - 1;
                }
            }
        }
        return cnt;
    }
    let text = String::from_utf8_lossy(&inbox);
    let mut n = 0u64;
    for seg in text.split('\n').filter(|s| !s.is_empty()) {
        if ts_is_greater(seg, cur) {
            break;
        }
        n += 1;
    }
    if n > total {
        0
    } else {
        n
    }
}

/// Opens the inbox fresh (a cached size can lag another box's append on a
/// shared home), reads up to the last `\n` only, and counts the lines at
/// index >= max(cursor, `woken_line`) that are to `handle` and not from it.
/// A `woken_line` past the inbox's end (it shrank) counts as never woken.
fn scan(comm_home: &Path, handle: &str, woken_line: u64) -> Scan {
    let cursor = cursor_offset(comm_home, handle);
    let Ok(bytes) = std::fs::read(comm_home.join("inbox").join(format!("{handle}.jsonl"))) else {
        return Scan::default();
    };
    let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Scan::default();
    };
    let total = bytes[..=end].iter().filter(|b| **b == b'\n').count() as u64;
    let woken_line = if woken_line > total { 0 } else { woken_line };
    let mut out = Scan { total, ..Scan::default() };
    for (i, line) in bytes[..end].split(|b| *b == b'\n').enumerate() {
        let i = i as u64;
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

/// The prompt glyphs of an agent the daemon can read, none for one it cannot.
/// Claude Code draws `❯`, or a bare `>` when its unicode check fails, which on
/// Windows depends on the environment. Codex ships OFF: no Codex screen has
/// been captured, and a glyph is never guessed, so a Codex row counts as a row
/// the daemon cannot type into.
fn prompt_glyphs(agent: &str, windows: bool) -> &'static [char] {
    match agent {
        "claude" if windows => &['\u{276f}', '>'],
        "claude" => &['\u{276f}'],
        _ => &[],
    }
}

/// A line of nothing but [`RULE`] (trailing spaces aside).
fn is_rule(line: &str) -> bool {
    let line = line.trim_end_matches(' ');
    !line.is_empty() && line.chars().all(|c| c == RULE)
}

/// The free-prompt test: the cursor sits on the empty input line. The row
/// indexes a real line, the line holds the glyph with only spaces before it,
/// and the cursor column is just after the glyph (or one more, over a space,
/// no-break space, tab or nothing). A dialog or a draft is not free. One
/// frame cannot tell a working row, whose input box is live too; the hold in
/// `wake_if_free` does. A bare `>` is a weak signal (quotes, diffs, shell
/// output), so on Windows, for either glyph, the lines directly above and below
/// must also be rules.
pub(crate) fn prompt_free(lines: &[String], cursor: Option<(u16, u16)>, agent: &str) -> bool {
    prompt_free_on(lines, cursor, agent, cfg!(windows))
}

pub(crate) fn prompt_free_on(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> bool {
    let glyphs = prompt_glyphs(agent, windows);
    let (false, Some((row, col))) = (glyphs.is_empty(), cursor) else {
        return false;
    };
    let row = row as usize;
    let Some(line) = lines.get(row) else {
        return false;
    };
    if windows {
        let boxed = row > 0 && is_rule(&lines[row - 1]) && lines.get(row + 1).is_some_and(|l| is_rule(l));
        if !boxed {
            return false;
        }
    }
    let cells: Vec<char> = line.chars().collect();
    let Some(g) = cells.iter().position(|c| glyphs.contains(c)) else {
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
        let mut checks = Vec::new();
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
            if prompt_glyphs(&agent, cfg!(windows)).is_empty() {
                continue;
            }
            let state_dir = crate::capsule_workspace::state_dir_for(&state_root, &ws.workspace_id);
            let home = comm_home.clone();
            let prior = woken.get(&handle).copied();
            let h = handle.clone();
            // Concurrent: each wake holds the screen for STILL_FOR, so N rows
            // with mail cost one hold per tick, not N.
            checks.push((handle, tokio::task::spawn_blocking(move || check_row(&home, &h, &state_dir, &agent, prior))));
        }
        for (handle, check) in checks {
            match check.await {
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
    // Only a Ready row is typed into: a row whose agent has ended can still
    // show a prompt-shaped last screen. (Nothing on the wake path restarts a
    // row; `wake_if_free` only attaches.)
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
        STILL_FOR,
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
        prompt_free_on(&lines(l), cur, "claude", false)
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
        for windows in [false, true] {
            assert!(!prompt_free_on(&lines(&["\u{203a}"]), Some((0, 1)), "codex", windows));
            assert!(!prompt_free_on(&lines(&["\u{276f}"]), Some((0, 1)), "codex", windows));
        }
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

    fn hashed(n: usize, inbox: &str) -> String {
        format!("{n} {}\n", line_hash(inbox.split('\n').nth(n - 1).unwrap().as_bytes()))
    }

    #[test]
    fn scan_counts_past_the_cursor() {
        let inbox = MINE.repeat(3);
        let d = home(&inbox, Some(&hashed(1, &inbox)));
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 3, unread: 2, fresh: 2 });
        // A wake at line 2 leaves one fresh line.
        assert_eq!(scan(d.path(), "a", 2), Scan { total: 3, unread: 2, fresh: 1 });
        // The inbox shrank past the last wake: every unread line is fresh.
        assert_eq!(scan(d.path(), "a", 9), Scan { total: 3, unread: 2, fresh: 2 });
    }

    #[test]
    fn cksum_matches_the_real_one() {
        // Values from the system `cksum`.
        assert_eq!(cksum(b""), (4294967295, 0));
        assert_eq!(cksum(b"a"), (1220704766, 1));
        assert_eq!(cksum(b"ab"), (2072780115, 2));
        assert_eq!(cksum(MINE.trim_end().as_bytes()), (3902411541, 31));
        assert_eq!(line_hash(b"a\0\n"), "1220704766-1");
    }

    fn off(inbox: &str, cursor: Option<&str>) -> u64 {
        cursor_offset(home(inbox, cursor).path(), "a")
    }

    const TS: &str = "{\"to\":\"a\",\"ts\":\"2026-01-0";

    fn ts_inbox(stamps: &[&str]) -> String {
        stamps.iter().map(|t| format!("{TS}{t}Z\"}}\n")).collect()
    }

    #[test]
    fn offsets_by_form() {
        let ib = MINE.repeat(3);
        // Count and hash matching; mismatching (one back); one past the end
        // with a hash (the total); further past (0); bare count past, in range.
        assert_eq!(off(&ib, Some(&hashed(2, &ib))), 2);
        assert_eq!(off(&ib, Some("2 1-1")), 1);
        assert_eq!(off(&ib, Some(&format!("4 {}", line_hash(b"x")))), 3);
        assert_eq!(off(&ib, Some("4")), 0);
        assert_eq!(off(&ib, Some("5 1-1")), 0);
        assert_eq!(off(&ib, Some("9")), 0);
        assert_eq!(off(&ib, Some("2")), 2);
        assert_eq!(off(&ib, Some("0")), 0);
        // Empty or absent.
        assert_eq!(off(&ib, Some("")), 0);
        assert_eq!(off(&ib, None), 0);
        assert_eq!(off("", Some("2")), 0);
    }

    #[test]
    fn timestamp_cursors() {
        let ib = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
        // Nothing newer: the total, so no unread and no wake storm.
        assert_eq!(off(&ib, Some("2026-01-02T00:00:00Z")), 3);
        // A newer line in the middle.
        assert_eq!(off(&ib, Some("2026-01-01T00:00:01Z")), 1);
        // An unparseable line before the boundary is counted, never a boundary.
        let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        assert_eq!(off(&torn, Some("2026-01-01T00:00:05Z")), 2);
        // A skewed earlier line: the FIRST greater line wins.
        let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
        assert_eq!(off(&skew, Some("2026-01-01T00:00:05Z")), 0);
        // A torn tail is a segment too, but a count past the total is 0.
        let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
        assert_eq!(off(&tail, Some("2026-01-02T00:00:00Z")), 0);
    }

    #[test]
    fn the_wake_reads_a_timestamp_cursor_as_no_mail() {
        let ib = ts_inbox(&["1T00:00:01", "1T00:00:02"]);
        let d = home(&ib, Some("2026-01-02T00:00:00Z\n"));
        assert_eq!(scan(d.path(), "a", 0).unread, 0);
    }

    /// The shell is the spec: every fixture is run through the real
    /// `sot_cursor_offset` and must give the same number. Linux only, like the
    /// other tests that run the shell: it needs bash and jq on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn agrees_with_the_shell() {
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/core/scripts/comm-lib.sh");
        let ib = MINE.repeat(3);
        let ts = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
        let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
        let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
        let nonobj = format!("{}5\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let arr = "{\"ts\":[1]}\n{\"ts\":\"2026-01-01T00:00:09Z\"}\n".to_string();
        let nul = format!("{{\"to\":\"a\",\"m\":\"x\0y\"}}\n{MINE}");
        let strs = format!("{}\"x\"\ntrue\n[1]\nnull\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let h2 = hashed(2, &ib);
        let h4 = format!("4 {}", line_hash(b"x"));
        let hn = hashed(1, &nul);
        let cases: Vec<(&str, Option<&str>)> = vec![
            (&ib, Some(&h2)),
            (&ib, Some("2 1-1")),
            (&ib, Some(&h4)),
            (&ib, Some("4")),
            (&ib, Some("5 1-1")),
            (&ib, Some("2")),
            (&ib, Some("0")),
            (&ib, Some("3 ")),
            (&ib, Some("")),
            (&ib, None),
            ("", Some("2")),
            (&nul, Some(&hn)),
            (&ts, Some("2026-01-02T00:00:00Z")),
            (&ts, Some("2026-01-01T00:00:01Z")),
            (&torn, Some("2026-01-01T00:00:05Z")),
            (&skew, Some("2026-01-01T00:00:05Z")),
            (&tail, Some("2026-01-02T00:00:00Z")),
            (&nonobj, Some("2026-01-01T00:00:05Z")),
            (&arr, Some("2026-01-01T00:00:05Z")),
            (&ib, Some("oops")),
            (&strs, Some("2026-01-01T00:00:05Z")),
        ];
        for (inbox, cursor) in cases {
            let d = home(inbox, cursor);
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(r#"source "$1"; sot_cursor_offset a 2>/dev/null"#)
                .arg("bash")
                .arg(&lib)
                .env("COMM_HOME", d.path())
                .env("SOT_COMM_HOME", d.path())
                .output()
                .expect("run bash");
            let shell: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("shell offset");
            assert_eq!(cursor_offset(d.path(), "a"), shell, "inbox {inbox:?} cursor {cursor:?}");
        }
    }

    #[test]
    fn scan_counts_a_missing_cursor_from_zero() {
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

    fn on(l: &[&str], cur: (u16, u16), windows: bool) -> bool {
        prompt_free_on(&lines(l), Some(cur), "claude", windows)
    }

    /// A claude row at rest, captured live on Linux (2026-10-01): cursor (8, 2), after a grey suggestion. Identifying text scrubbed.
    const LINUX_IDLE: [&str; 13] = [
        "● Monitor event: a watched row changed state                                                                     │",
        "                                                                                                                 │",
        "● Nothing to act on; staying idle.                                                                               │",
        "  Waiting for the next case.                                                                                     │",
        "                                                                                                                 │",
        "✻ Sautéed for 3s · done 9:21 AM · 2 monitors still running                                                       │",
        "                                                                         ✔ Update installed · Restart to update  │",
        "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
        "❯\u{a0}ready for the next case",
        "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
        "  Opus 5.5 [00000000] acct ·think:xhigh | v2.1.285 | demo:main | 0 uncommitted",
        "  Session: 1k (in:1k out:1k) | $0.00",
        "  ⏵⏵ auto mode on · 2 monitors · ← for agents",
    ];

    // Windows: REAL captures of a claude row mid-turn on Windows (2026-10-01, Claude Code 2.1.282, the row's own
    // screen read, trailing spaces trimmed by the reader), statusline id and cost scrubbed. rows 75, cols 203,
    // cursor (69, 2) = row 7 here, col 2. The prompt is the single byte '>' and the rules are U+2500 x 203: Claude
    // Code's `figures` fallback, chosen when its unicode check fails (on Windows: no WT_SESSION, TERM other than
    // xterm-256color, ...); the statusline's '√' is the same fallback for '✔'.
    const WIN_RULE: &str = "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────";
    /// Mid-turn: the spinner and the running tool are live above the box.
    const WIN_TURN_A: [&str; 13] = [
        "  ⎿  Running… (8s)",
        "     (ctrl+b to run in background)",
        "",
        "✢ Prestidigitating… (38s · ↓ 584 tokens)",
        "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
        "",
        WIN_RULE,
        ">",
        WIN_RULE,
        "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
        "  Session: 197k (in:197k out:471) | $0.00",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        "",
    ];
    /// DERIVED from WIN_TURN_A (not captured): the turn ended; lines 0, 1 and 3 replaced by finished-turn text.
    const WIN_REST: [&str; 13] = [
        "  ⎿  Done.",
        "",
        "",
        "✻ Worked for 38s",
        "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
        "",
        WIN_RULE,
        ">",
        WIN_RULE,
        "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
        "  Session: 197k (in:197k out:471) | $0.00",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        "",
    ];
    /// DERIVED from WIN_REST: a quoted ">" output line at row 1 (put the cursor on it at (1, 2)).
    const WIN_QUOTED: [&str; 13] = [
        "  ⎿  Done.",
        "> quoted text",
        "",
        "✻ Worked for 38s",
        "  ⎿  Tip: Hit shift+tab to cycle between manual mode, auto-accept edit mode, and plan mode",
        "",
        WIN_RULE,
        ">",
        WIN_RULE,
        "  Opus 5.5 (1M context) [00000000] think:medium | v2.1.282 | demo:main | 0 uncommitted                                                                    √ Update installed · Restart to update",
        "  Session: 197k (in:197k out:471) | $0.00",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        "",
    ];

    #[test]
    fn linux_rest_capture_is_free() {
        assert!(on(&LINUX_IDLE, (8, 2), false));
    }

    #[test]
    fn a_windows_turn_frame_passes_the_box_check() {
        assert!(on(&WIN_TURN_A, (7, 2), true));
    }

    #[test]
    fn windows_takes_both_glyphs_inside_the_box() {
        assert!(on(&WIN_REST, (7, 2), true));
        let mut bare = WIN_REST;
        bare[7] = "\u{276f}";
        assert!(on(&bare, (7, 2), true));
        assert!(on(&LINUX_IDLE, (8, 2), true));
    }

    #[test]
    fn windows_quoted_lines_are_not_free() {
        assert!(!on(&WIN_QUOTED, (1, 2), true));
        let mut quoted = WIN_QUOTED;
        quoted[1] = "\u{276f} quoted text";
        assert!(!on(&quoted, (1, 2), true));
    }

    #[test]
    fn linux_never_takes_gt() {
        assert!(!on(&WIN_REST, (7, 2), false));
    }
}
