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
//! holding: a row is typed into only if the rows through the input box's lower
//! rule stay identical for [`STILL_FOR`] (a working row's spinner, always above
//! the box, redraws several times a second). The rows below the box are not
//! watched: Claude Code draws a background-agent footer there that ticks every
//! second at rest.
//! A row whose registry entry carries a `stop_at` mark under [`STOP_HOOK_BOUND`]
//! old is running its Stop hook, which shows a free, still prompt; once the mark
//! has landed the row is skipped, asked before the hold and again after it. The
//! hook's 1 s lock wait bounds the lock, not the write; the mark lands about
//! 0.13 s after the hook starts on Linux, inside the 1.5 s hold, but that is not
//! guaranteed.
//! The prompt glyph is `❯`, or on Windows either `❯` or `>` (Claude Code's
//! fallback when its unicode check fails, which on Windows depends on the
//! environment). On both OSes the line sits directly between the input box's
//! two rules and the glyph is followed by U+00A0, the main prompt's own mark
//! (menus and dialog inputs draw an ASCII space); on Windows a bare glyph also
//! counts until the NBSP is shown to survive ConPTY (`NBSP_ON_WINDOWS`). A row at rest whose
//! rows through the box never hold still (a live clock, an animation) is never woken, and neither is a row
//! whose Stop ended without `stop` (Esc, an API error, a killed hook) for at most
//! the bound + 1 s + [`TICK`] + [`STILL_FOR`]; this fails closed and its end-of-turn check still reads the mail.
//! The converse fails open: a working row with no fresh mark (no hooks, the mark
//! not written or not yet landed, a hook run past the bound, a mark more than 1 s ahead (a backward
//! clock step)) whose rows through the box hold still
//! for [`STILL_FOR`] is typed into, and the line lands as a queued message.
//! A permission menu is not free by construction (its options use an ASCII
//! space and it is never boxed), though it holds still. The menu fixtures are
//! synthetic (built from the bundle's layout; no menu screen has been
//! captured), and this assumes the cursor follows focus into a dialog. The Windows-only gap:
//! while the NBSP is not required there, an agents-view task box with an empty
//! placeholder (a voice state) reads free. Known test gap: no test covers the
//! re-check after the hold (`wake_if_free` asks `is_free` again before typing),
//! nor `check_row`'s registry-then-clock order; deleting either passes every
//! test, and a test that does not flake needs a seam.
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
/// The Stop hook's longest run (its auditor call is capped at 45 s; Codex kills the hook at 10 s). A row whose
/// registry entry carries a `stop_at` mark at most this old is running its Stop hook and is not typed into; an
/// older mark is a Stop that never ended (Esc, an API error, a killed hook).
const STOP_HOOK_BOUND: Duration = Duration::from_secs(60);
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

/// Whether the registry says `handle`'s Stop hook is running: its `stop_at` mark is at most [`STOP_HOOK_BOUND`]
/// old and at most 1 s ahead of `now_secs`. The caller takes `now_secs` after reading the registry, so on a steady clock
/// a mark the read saw is never ahead and the 1 s is slack; a mark further ahead (a backward clock step) holds nothing.
/// On a steady clock no mark holds a row past the bound + 1 s (the stamp is cut to the second); a backward step after
/// the stamp extends the hold by the step. Both ends are `%Y-%m-%dT%H:%M:%SZ`, so string order is time order.
fn stop_hook_running(registry: &[u8], handle: &str, now_secs: u64) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(registry) else {
        return false;
    };
    let Some(at) = v.get("agents").and_then(|a| a.get(handle)).and_then(|e| e.get("stop_at")).and_then(|s| s.as_str()) else {
        return false;
    };
    let iso = crate::handlers::iso8601_utc_from_secs;
    let bound = STOP_HOOK_BOUND.as_secs();
    at >= iso(now_secs.saturating_sub(bound)).as_str() && at <= iso(now_secs + 1).as_str()
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

/// Whether the input prompt's own separator, U+00A0 after the glyph, is required on Windows. On Linux it always
/// is. Off until a Windows screen read shows the NBSP survives ConPTY; while off, a bare glyph also counts there,
/// and an agents-view task box with an empty placeholder (a voice state) reads free: a named Windows-only gap.
const NBSP_ON_WINDOWS: bool = false;

fn nbsp_required(windows: bool) -> bool {
    !windows || NBSP_ON_WINDOWS
}

/// The free-prompt test, all of: (a) only spaces before the glyph; (b) the cursor
/// is just after the glyph, or one more; (c) the line sits directly between two
/// rule lines, the input box's; (d) the glyph is followed by U+00A0, the main
/// prompt's own mark (menus and dialog inputs draw an ASCII space), or, where
/// the NBSP is not required ([`NBSP_ON_WINDOWS`]), by nothing but spaces. A
/// menu, a dialog or a draft is not free. One frame cannot tell a working row,
/// whose input box is live too; the hold in `wake_if_free` does.
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
    let boxed = row > 0 && is_rule(&lines[row - 1]) && lines.get(row + 1).is_some_and(|l| is_rule(l));
    if !boxed {
        return false;
    }
    let cells: Vec<char> = line.chars().collect();
    let Some(g) = cells.iter().position(|c| glyphs.contains(c)) else {
        return false;
    };
    let col = col as usize;
    let nbsp = cells.get(g + 1) == Some(&'\u{a0}');
    let bare = !nbsp_required(windows) && cells[g + 1..].iter().all(|c| *c == ' ');
    cells[..g].iter().all(|c| *c == ' ') && (col == g + 1 || col == g + 2) && (nbsp || bare)
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
    let free = |l: &[String], c: Option<(u16, u16)>, a: &str| {
        // The registry, then the clock: a mark the read sees was stamped no later than `now`.
        let registry = crate::handlers::read_registry_fresh(&home.join("registry.json")).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        prompt_free(l, c, a) && !stop_hook_running(&registry, handle, now)
    };
    match wake_if_free(
        state_dir,
        CONTROLLER_ID,
        WAKE_LINE,
        &free,
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
    use crate::capsule_workspace::headless::held_rows;

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn free(l: &[&str], cur: Option<(u16, u16)>) -> bool {
        prompt_free_on(&lines(l), cur, "claude", false)
    }

    const R: &str = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}";

    fn boxed(line: &str) -> Vec<String> {
        vec![R.to_string(), line.to_string(), R.to_string()]
    }

    fn bfree(line: &str, col: u16, windows: bool) -> bool {
        prompt_free_on(&boxed(line), Some((1, col)), "claude", windows)
    }

    #[test]
    fn free_prompts() {
        for windows in [false, true] {
            // Ghost-text suggestion with the cursor at its start.
            assert!(bfree("\u{276f}\u{a0}try this", 2, windows));
        }
    }

    #[test]
    fn drafts_dialogs_and_missing_cursors_are_not_free() {
        for w in [false, true] {
            // The cursor at the draft's end (clause b), and text before the glyph (clause a).
            assert!(!bfree("\u{276f}\u{a0}hello", 7, w));
            assert!(!bfree("x\u{276f}\u{a0}", 3, w));
        }
        assert!(!free(&["\u{276f}", "Allow this action? (y/n)"], Some((1, 24))));
        assert!(!free(&["\u{276f}"], None));
        assert!(!free(&["\u{276f}"], Some((5, 1))));
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

    /// A claude row mid-turn, captured live on Linux (2026-10-01): the spinner is live above the box and the cursor still sits at (8, 2). Line 2 is a queued input echoed with the glyph OUTSIDE the box.
    const LINUX_TURN_A: [&str; 13] = [
        "     (ctrl+b to run in background)                                                                               │",
        "                                                                                                                 │",
        "❯ [sot-comm] you have mail: run comm-poll.sh                                                                     │",
        "  ctrl+x ctrl+s to send now                                                                                      │",
        "                                                                                                                 │",
        "✢ Levitating… (56s · ↓ 3.4k tokens)                                                                              │",
        "                                                                         ✔ Update installed · Restart to update  │",
        "───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
        "❯\u{a0}Press up to edit queued messages",
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

    /// Two live captures of a claude row at rest, scrubbed, one second apart: a background agent's footer below the box ticks (`3m 9s` to `3m 11s`); every other row is identical.
    fn footer_frame(text: &str) -> Vec<String> {
        text.lines().filter(|l| !l.starts_with("cols=")).map(String::from).collect()
    }

    #[test]
    fn the_agent_footer_is_not_watched() {
        let a = footer_frame(include_str!("../tests/fixtures/comm_wake/footer-frame-a.txt"));
        let b = footer_frame(include_str!("../tests/fixtures/comm_wake/footer-frame-b.txt"));
        assert_eq!((a.len(), b.len()), (75, 75));
        for windows in [false, true] {
            assert!(prompt_free_on(&a, Some((67, 2)), "claude", windows));
        }
        assert_ne!(a, b, "the whole-frame hold saw the footer tick");
        assert_eq!(held_rows(&a, 67), held_rows(&b, 67));
        assert_eq!(held_rows(&a, 67).len(), 69);
        assert!(is_rule(&held_rows(&a, 67)[68]));
    }

    #[test]
    fn the_spinner_above_the_box_is_watched() {
        let mut linux = LINUX_TURN_A;
        linux[5] = "✢ Levitating… (57s · ↓ 3.4k tokens)";
        assert_ne!(held_rows(&lines(&linux), 8), held_rows(&lines(&LINUX_TURN_A), 8));
        let mut win = WIN_TURN_A;
        win[3] = "✢ Prestidigitating… (39s · ↓ 584 tokens)";
        assert_ne!(held_rows(&lines(&win), 7), held_rows(&lines(&WIN_TURN_A), 7));
    }

    #[test]
    fn a_stop_mark_holds_only_inside_the_bound() {
        let now = 1_800_000_000;
        let iso = crate::handlers::iso8601_utc_from_secs;
        let at = |off: i64| iso((now as i64 + off) as u64);
        let reg = |entry: &str| format!(r#"{{"agents":{{"h":{entry}}}}}"#).into_bytes();
        let mark = |off: i64| reg(&format!(r#"{{"floor":"user","stop_at":"{}"}}"#, at(off)));
        for off in [0, -60, 1] {
            assert!(stop_hook_running(&mark(off), "h", now), "offset {off}");
        }
        for off in [-61, 2, 60] {
            assert!(!stop_hook_running(&mark(off), "h", now), "offset {off}");
        }
        assert!(!stop_hook_running(&reg(r#"{"floor":"user"}"#), "h", now));
        assert!(!stop_hook_running(&reg(r#"{"stop_at":null}"#), "h", now));
        assert!(!stop_hook_running(&reg(r#"{"stop_at":5}"#), "h", now));
        assert!(!stop_hook_running(&mark(0), "other", now));
        assert!(!stop_hook_running(b"", "h", now));
        assert!(!stop_hook_running(b"{", "h", now));
    }

    #[test]
    fn one_turn_frame_reads_free() {
        // One frame of a working row reads free (its box shows the glyph, NBSP and a queued-message hint); only
        // the hold in `wake_if_free` protects it (itest `a_working_row_is_not_typed_into_until_it_rests`).
        assert!(on(&LINUX_TURN_A, (8, 2), false));
        assert_eq!(on(&WIN_TURN_A, (7, 2), true), !nbsp_required(true));
        let mut nbsp = WIN_TURN_A;
        nbsp[7] = ">\u{a0}";
        assert!(on(&nbsp, (7, 2), true));
    }

    #[test]
    fn windows_takes_both_glyphs_inside_the_box() {
        assert_eq!(on(&WIN_REST, (7, 2), true), !nbsp_required(true));
        let mut nbsp = WIN_REST;
        nbsp[7] = ">\u{a0}";
        assert!(on(&nbsp, (7, 2), true));
        let mut bare = WIN_REST;
        bare[7] = "\u{276f}";
        assert_eq!(on(&bare, (7, 2), true), !nbsp_required(true));
        bare[7] = "\u{276f}\u{a0}";
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

    /// SYNTHETIC, from the layout of Claude Code's permission dialog: every option is `[pointer-or-space, " ", label]`.
    const PERM_MENU: [&str; 10] = [
        "Bash command",
        "",
        "  rm -rf build",
        "  Remove the build directory",
        "",
        "Do you want to proceed?",
        "",
        "\u{276f} 1. Yes",
        "  2. Yes, and don't ask again for rm commands",
        "  3. No, and tell Claude what to do differently (esc)",
    ];
    /// SYNTHETIC, from the layout of the AskUserQuestion dialog.
    const ASKQ_MENU: [&str; 6] = [
        "Which approach?",
        "",
        "\u{276f} 1. Fast path",
        "     Skip the cache",
        "  2. Safe path",
        "  3. Type something.",
    ];

    fn with_rules<const N: usize>(menu: [&str; N], rows: [usize; 2]) -> [&str; N] {
        let mut m = menu;
        m[rows[0]] = R;
        m[rows[1]] = R;
        m
    }

    #[test]
    fn menus_are_never_free() {
        let perm_boxed = with_rules(PERM_MENU, [6, 8]);
        let askq_boxed = with_rules(ASKQ_MENU, [1, 3]);
        for windows in [false, true] {
            assert!(!on(&PERM_MENU, (7, 2), windows));
            assert!(!on(&perm_boxed, (7, 2), windows));
            assert!(!on(&ASKQ_MENU, (2, 2), windows));
            assert!(!on(&askq_boxed, (2, 2), windows));
        }
        let mut perm_gt = PERM_MENU;
        perm_gt[7] = "> 1. Yes";
        let mut perm_gt_boxed = perm_boxed;
        perm_gt_boxed[7] = "> 1. Yes";
        let mut askq_gt = ASKQ_MENU;
        askq_gt[2] = "> 1. Fast path";
        let mut askq_gt_boxed = askq_boxed;
        askq_gt_boxed[2] = "> 1. Fast path";
        assert!(!on(&perm_gt, (7, 2), true));
        assert!(!on(&perm_gt_boxed, (7, 2), true));
        assert!(!on(&askq_gt, (2, 2), true));
        assert!(!on(&askq_gt_boxed, (2, 2), true));
        assert!(!free(&["\u{276f} 1. Yes"], Some((0, 2))));
    }

    #[test]
    fn dialog_inputs_are_never_free() {
        for windows in [false, true] {
            let ask = "\u{276f} press 1-3 or type your answer";
            assert!(!prompt_free_on(&lines(&[ask]), Some((0, 2)), "claude", windows));
            assert!(!bfree(ask, 2, windows));
            assert!(!bfree("\u{276f} describe a task for a new session", 2, windows));
        }
    }

    #[test]
    fn an_empty_idle_prompt_as_the_wake_reads_it_is_free() {
        let mut idle = LINUX_IDLE;
        idle[8] = "\u{276f}\u{a0}";
        for windows in [false, true] {
            assert!(on(&idle, (8, 2), windows));
            assert!(on(&idle, (8, 1), windows));
            assert!(bfree("\u{276f}\u{a0}", 2, windows));
            assert!(bfree("\u{276f}\u{a0}", 1, windows));
        }
    }

    #[test]
    fn a_suggestion_row_is_free() {
        for windows in [false, true] {
            assert!(on(&LINUX_IDLE, (8, 2), windows));
        }
        assert!(bfree(">\u{a0}text", 2, true));
        assert!(!bfree(">\u{a0}text", 2, false));
    }

    #[test]
    fn a_glyph_then_a_space_is_not_free() {
        for windows in [false, true] {
            assert!(!bfree("\u{276f} text", 2, windows));
        }
        assert!(!bfree("> text", 2, true));
    }

    #[test]
    fn output_lines_are_not_free() {
        // The real queued-input echo of a working row: the glyph sits outside the box.
        for windows in [false, true] {
            assert!(!on(&LINUX_TURN_A, (2, 2), windows));
        }
    }

    #[test]
    fn a_bare_glyph_follows_the_switch() {
        for windows in [false, true] {
            assert_eq!(bfree("\u{276f}", 1, windows), !nbsp_required(windows));
            assert_eq!(bfree("\u{276f}", 2, windows), !nbsp_required(windows));
            // The agents-view task box with an empty placeholder (a voice state) reads as a bare glyph: free
            // wherever the NBSP is not required, the named Windows-only gap.
            assert_eq!(bfree("\u{276f}", 2, windows), !nbsp_required(windows));
        }
    }
}
