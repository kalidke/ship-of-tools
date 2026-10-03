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
//! Nothing but spaces may follow the NBSP, and the free test reads the cursor's row with dim cells blank
//! (`headless::free_test_lines`): Claude Code draws its suggestion and placeholders dim, so they read empty,
//! while a typed draft is not dim and reads not free wherever its cursor sits. The input must reach the main
//! agent: no agents panel below the box, or a panel with no panel cursor and its one dot on main
//! ([`panel_refusal`]); Claude Code's footer is not read. A pane too narrow for the wake line on the prompt's one
//! row refuses ([`fits`]). A row with unread mail that the wake refuses for [`REFUSED_FOR`] gets one log line per run of
//! refusals, naming its handle, the reason and the line above the prompt. Known limits: a suggestion drawn by colour rather than dim reads as a draft, and so does every
//! suggestion on Windows until a screen read shows SGR 2 survives ConPTY; a statusline that draws `●` or `◯`
//! below the box refuses the row; with no panel nothing below the box is checked, so a view of another agent
//! or a focus off the input that drew no panel would read free (every captured view draws the panel); with agent view on, a
//! focus on the panel that draws nothing reads free ([`panel_refusal`]).
//! Enter goes only after a screen read shows the typed line alone in main's input box ([`typed_refusal`]);
//! otherwise no Enter goes, the
//! attempt counts as the wake (the line is not typed again before new mail or [`REPEAT_AFTER`]), and a warning names
//! it. That gate withholds Enter after a stray key between the
//! final read and the typing; the line itself has then gone, without Enter, wherever that key put focus (the
//! panel or a draft). Nothing guards the window between the gate's read and the Enter: a key pressed, or a dialog
//! or permission prompt drawn, in it receives the Enter. A later wake refuses for whatever the screen then
//! shows, and the refusal streak logs it.
//!
//! "Last woken" lives in the tick task's memory, never on disk, so a daemon
//! restart wakes every row with unread mail once, at its first free prompt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::capsule_workspace::headless::{wake_if_free, HeadlessError, WakeOutcome};
use crate::workspaces::Workspaces;

pub const TICK: Duration = Duration::from_secs(2);
/// Mail still unread this long after a wake gets one more line.
const REPEAT_AFTER: Duration = Duration::from_secs(600);
/// A row with unread mail that the wake has refused this long is logged, once per run of refusals, so a prompt
/// the free test does not recognise is not silent.
const REFUSED_FOR: Duration = Duration::from_secs(60);
/// The Stop hook's longest run (its auditor call is capped at 45 s; Codex kills the hook at 10 s). A row whose
/// registry entry carries a `stop_at` mark at most this old is running its Stop hook and is not typed into; an
/// older mark is a Stop that never ended (Esc, an API error, a killed hook).
const STOP_HOOK_BOUND: Duration = Duration::from_secs(60);
const WAKE_LINE: &str = "[sot-comm] you have mail: run comm-poll.sh";
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

/// An input-box border: a rule, or a rule with one ` label ` set into it, as Claude Code draws a named
/// session's box top (`───── name ─`; seen 2026-10-02, v2.1.285) and the box of an agent being viewed
/// (`──── task ─`, v2.1.287). A label does not say whose box it is; [`panel_refusal`] does. Known limits, both
/// fail closed: a label containing `─` is not a border, and neither is a label as wide as the line (no rule
/// left on one side).
fn is_border(line: &str) -> bool {
    let line = line.trim_end_matches(' ');
    if is_rule(line) {
        return true;
    }
    let Some(inner) = line.strip_prefix(RULE).and_then(|l| l.strip_suffix(RULE)) else {
        return false;
    };
    let label = inner.trim_matches(RULE);
    label.len() > 2 && label.starts_with(' ') && label.ends_with(' ') && !label.contains(RULE)
}

/// Whether the input prompt's own separator, U+00A0 after the glyph, is required on Windows. On Linux it always
/// is. Off until a Windows screen read shows the NBSP survives ConPTY; while off, a bare glyph also counts there,
/// and an agents-view task box with an empty placeholder (a voice state) reads free: a named Windows-only gap.
const NBSP_ON_WINDOWS: bool = false;

fn nbsp_required(windows: bool) -> bool {
    !windows || NBSP_ON_WINDOWS
}

/// The agents panel's mark for the agent the input reaches, and for every other agent.
const PANEL_DOT: char = '\u{25cf}';
const PANEL_RING: char = '\u{25ef}';

/// Why the screen is not a free prompt for the wake; `None` when it is. Free is all of: (a) only spaces before
/// the glyph; (b) the cursor just after the glyph, or one more; (c) the line directly between the input box's
/// two borders (a rule, optionally labelled); (d) after the glyph U+00A0, the main prompt's own mark (menus and
/// dialog inputs draw an ASCII space), then nothing but spaces, or, where the NBSP is not required
/// ([`NBSP_ON_WINDOWS`]), nothing but spaces at all; (e) the input reaches the main agent ([`panel_refusal`]); (f) the wake line fits on the prompt row ([`fits`]).
/// The wake reads the cursor's row with dim cells blank (`headless::free_test_lines`), so Claude Code's dim
/// suggestion or placeholder reads empty and a typed draft reads not free wherever its cursor sits. One frame
/// cannot tell a working row, whose input box is live too; the hold in `wake_if_free` does.
fn refused_on(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> Option<&'static str> {
    match input_refused(lines, cursor, agent, windows, Expect::Empty) {
        // Any refusal while the box holds the wake's own line: an earlier wake typed it and did not send it.
        Some(_) if typed_refusal(lines, cursor, agent, windows, WAKE_LINE).is_none() => Some("wake text left unsent"),
        None if !fits(lines, cursor, WAKE_LINE) => Some("pane too narrow for the wake line"),
        other => other,
    }
}

/// Whether `text`, typed after the glyph and its NBSP, leaves two columns before the end of the box's lower rule,
/// which Claude Code draws the pane's full width: one for the cursor after the text, one spare, as the column at
/// which Claude Code wraps its input has not been measured. The gate before Enter reads the prompt's one row, so a
/// line that would wrap is never typed. Asked only once [`input_refused`] has passed, so the cursor's row lies
/// between two borders.
fn fits(lines: &[String], cursor: Option<(u16, u16)>, text: &str) -> bool {
    let Some((row, _)) = cursor else {
        return false;
    };
    let row = row as usize;
    let width = lines[row + 1].trim_end_matches(' ').chars().count();
    let glyph = lines[row].chars().take_while(|c| *c == ' ').count();
    glyph + 2 + text.chars().count() + 2 <= width
}

/// What the input box is expected to hold: nothing (the free test), or exactly the typed wake line (the gate
/// before Enter).
enum Expect<'a> {
    Empty,
    Typed(&'a str),
}

/// Why `text`, just typed, does not sit alone in main's input box (`None` when it does): every structural check of [`refused_on`] (a box,
/// the glyph, nothing before it, [`panel_refusal`] below) except the cursor column, then after the glyph the
/// NBSP (a space where the NBSP is not required), exactly `text`, and spaces. Enter goes only when this holds.
pub(crate) fn typed_refusal(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool, text: &str) -> Option<&'static str> {
    input_refused(lines, cursor, agent, windows, Expect::Typed(text))
}

fn input_refused(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool, expect: Expect) -> Option<&'static str> {
    let glyphs = prompt_glyphs(agent, windows);
    if glyphs.is_empty() {
        return Some("no wake predicate for this agent");
    }
    let Some((row, col)) = cursor else {
        return Some("no cursor");
    };
    let (row, col) = (row as usize, col as usize);
    let boxed = row > 0 && row + 1 < lines.len() && is_border(&lines[row - 1]) && is_border(&lines[row + 1]);
    if !boxed {
        return Some("not in an input box");
    }
    let cells: Vec<char> = lines[row].chars().collect();
    let Some(g) = cells.iter().position(|c| glyphs.contains(c)) else {
        return Some("no prompt glyph");
    };
    if !cells[..g].iter().all(|c| *c == ' ') {
        return Some("text before the glyph");
    }
    let rest = &cells[g + 1..];
    let blank = |cs: &[char]| cs.iter().all(|c| *c == ' ');
    match expect {
        Expect::Empty => {
            if col != g + 1 && col != g + 2 {
                return Some("cursor not at the prompt");
            }
            let empty = (rest.first() == Some(&'\u{a0}') && blank(&rest[1..])) || (!nbsp_required(windows) && blank(rest));
            if !empty {
                return Some("input not empty");
            }
        }
        Expect::Typed(text) => {
            let after = match rest.first() {
                Some('\u{a0}') => &rest[1..],
                Some(' ') if !nbsp_required(windows) => &rest[1..],
                _ => return Some("typed text not in main's input box"),
            };
            let want: Vec<char> = text.chars().collect();
            if !after.starts_with(&want) || !blank(&after[want.len()..]) {
                return Some("typed text not in main's input box");
            }
        }
    }
    panel_refusal(&lines[row + 2..])
}

/// Whether the input reaches the main agent, read from the rows below the box. If no line there carries
/// [`PANEL_DOT`] or [`PANEL_RING`], there is no agents panel and it does. Otherwise every panel line starts with two
/// spaces (a focused panel draws `❯ ` on the line under its cursor) and the one dotted line is `  ● main`. Claude
/// Code's footer is not read: its wording changes with the version, the permission mode, the background tasks and
/// the width (2.1.288 drops `(shift+tab to cycle)` after the first shift+tab), so it is no evidence of focus. With
/// agent view off, as the daemon runs every new capsule row, focus on the panel draws the panel cursor at once and
/// moves the terminal cursor off the prompt (Claude Code 2.1.288, 2026-10-03). Known gap, fails open: with agent view
/// on, 2.1.287's first ↓ moves focus to the panel and draws nothing (capture 05), and that frame reads free; there
/// the line types into the prompt and, if it shows, Enter opens the selected footer item rather than sending it
/// (Claude Code docs, keybindings, "Footer actions"), so the line is left unsent.
fn panel_refusal(below: &[String]) -> Option<&'static str> {
    let panel: Vec<&str> = below.iter().map(|l| l.trim_end_matches(' ')).filter(|l| l.contains(PANEL_DOT) || l.contains(PANEL_RING)).collect();
    if panel.is_empty() {
        return None;
    }
    if panel.iter().any(|l| !l.starts_with("  ")) {
        return Some("agents panel focused or unrecognised");
    }
    let dotted: Vec<&str> = panel.iter().copied().filter(|l| l.contains(PANEL_DOT)).collect();
    if dotted != ["  \u{25cf} main"] {
        return Some("the panel's dot is not on main");
    }
    None
}

/// The tick. Runs forever; started once from `server::run`.
pub async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let mut woken: HashMap<String, Woken> = HashMap::new();
    let mut streaks: HashMap<String, Streak> = HashMap::new();
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
            if let Ok(step) = check.await {
                settle(&mut woken, &mut streaks, handle, step, Instant::now());
            }
        }
    }
}

enum Step {
    Clear,
    Skip,
    Woke(Woken),
    /// The screen was not a free prompt, or did not hold still.
    Refused(Refusal),
}

/// Why the wake did not type into a row with mail: a [`refused_on`] reason, `stop hook running`, `moved during the
/// hold` or `text not confirmed`, and the line above the cursor's row (the box's top border, when there is a box).
#[derive(Debug)]
struct Refusal {
    reason: &'static str,
    border: String,
    /// An unconfirmed text write's phase and error text. Warned at once, once per streak: a write that keeps failing
    /// is tried again every tick.
    detail: Option<String>,
}

/// A row with mail that the wake keeps refusing: when the run of refusals began, and whether its one line is out.
struct Streak {
    since: Instant,
    logged: bool,
    /// Whether this streak's unconfirmed-write warning is out.
    warned: bool,
}

/// One row's tick result into the task's memory. A wake or a read inbox ends the row's refusal streak.
fn settle(woken: &mut HashMap<String, Woken>, streaks: &mut HashMap<String, Streak>, handle: String, step: Step, now: Instant) {
    match step {
        Step::Clear => {
            woken.remove(&handle);
            streaks.remove(&handle);
        }
        Step::Woke(w) => {
            streaks.remove(&handle);
            woken.insert(handle, w);
        }
        Step::Refused(r) => {
            let s = streaks.entry(handle.clone()).or_insert(Streak { since: now, logged: false, warned: false });
            if let (Some(detail), false) = (&r.detail, s.warned) {
                s.warned = true;
                tracing::warn!(handle = %handle, border = ?r.border, "comm wake: {} ({detail})", r.reason);
            }
            if !s.logged && now.duration_since(s.since) >= REFUSED_FOR {
                s.logged = true;
                tracing::info!(handle = %handle, reason = ?r.reason, border = ?r.border, "comm wake: a row with unread mail keeps refusing the wake");
            }
        }
        Step::Skip => {}
    }
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
    let seen: std::cell::RefCell<(Option<&'static str>, String)> = Default::default();
    let free = |l: &[String], c: Option<(u16, u16)>, a: &str| {
        // The registry, then the clock: a mark the read sees was stamped no later than `now`.
        let registry = crate::handlers::read_registry_fresh(&home.join("registry.json")).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let reason = refused_on(l, c, a, cfg!(windows)).or_else(|| stop_hook_running(&registry, handle, now).then_some("stop hook running"));
        let border = c.and_then(|(row, _)| l.get((row as usize).checked_sub(1)?)).cloned().unwrap_or_default();
        *seen.borrow_mut() = (reason, border);
        reason.is_none()
    };
    let out = wake_if_free(state_dir, CONTROLLER_ID, WAKE_LINE, &free, agent, STILL_FOR, OP_BUDGET, QUIET_BUDGET, PACING_BUDGET);
    step_of(handle, out, seen.take(), s.total, Instant::now())
}

/// What one wake attempt means for the row. A line that was typed counts as the wake whether or not Enter followed or
/// was confirmed (ADR 0049: one line per batch): typing it again would repeat it wherever focus went, or send it twice.
/// A text write that failed or whose delivery is unknown is a refusal, decided by the next tick's screen read and
/// warned once per streak ([`settle`]). An attach or checkpoint failure is no row this tick.
fn step_of(handle: &str, out: Result<WakeOutcome, HeadlessError>, seen: (Option<&'static str>, String), total: u64, now: Instant) -> Step {
    let (reason, border) = seen;
    match out {
        Ok(WakeOutcome::Woke) => Step::Woke(Woken { line: total, at: now }),
        Ok(WakeOutcome::NotFree) => Step::Refused(Refusal { reason: reason.unwrap_or("moved during the hold"), border, detail: None }),
        Ok(WakeOutcome::TypedNoEnter { reason, border }) => {
            tracing::warn!(handle, border = ?border, "comm wake: typed the line but it did not show in main's input box ({reason}); no Enter sent");
            Step::Woke(Woken { line: total, at: now })
        }
        Ok(WakeOutcome::Unconfirmed { step: "enter", detail }) => {
            tracing::warn!(handle, border = ?border, "comm wake: enter not confirmed ({detail}); the line was typed, so it is not typed again");
            Step::Woke(Woken { line: total, at: now })
        }
        Ok(WakeOutcome::Unconfirmed { detail, .. }) => Step::Refused(Refusal { reason: "text not confirmed", border, detail: Some(detail) }),
        Err(e) => {
            tracing::debug!(handle, phase = e.phase, detail = %e.detail, "comm wake: row not typeable this tick");
            Step::Skip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule_workspace::headless::{free_test_lines, held_rows};

    fn prompt_free_on(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> bool {
        refused_on(lines, cursor, agent, windows).is_none()
    }

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn free(l: &[&str], cur: Option<(u16, u16)>) -> bool {
        prompt_free_on(&lines(l), cur, "claude", false)
    }

    const R: &str = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}";

    fn boxed(line: &str) -> Vec<String> {
        vec![rule80(), line.to_string(), rule80()]
    }

    #[test]
    fn a_box_holding_the_wake_line_names_it() {
        let held = format!("\u{276f}\u{a0}{WAKE_LINE}");
        assert_eq!(refused_on(&boxed(&held), Some((1, 2)), "claude", false), Some("wake text left unsent"));
        // As a wake leaves it: the cursor at the END of the text.
        let end = 2 + WAKE_LINE.chars().count() as u16;
        assert_eq!(refused_on(&boxed(&held), Some((1, end)), "claude", false), Some("wake text left unsent"));
        assert_eq!(refused_on(&boxed("\u{276f}\u{a0}hello"), Some((1, 2)), "claude", false), Some("input not empty"));
    }

    fn bfree(line: &str, col: u16, windows: bool) -> bool {
        prompt_free_on(&boxed(line), Some((1, col)), "claude", windows)
    }

    #[test]
    fn free_prompts() {
        for windows in [false, true] {
            // A suggestion as the wake reads it: the dim text blank.
            assert!(bfree("\u{276f}\u{a0}", 2, windows));
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
        // The grey suggestion as the wake reads it: blank.
        let mut idle = LINUX_IDLE;
        idle[8] = "\u{276f}\u{a0}";
        assert!(on(&idle, (8, 2), false));
    }

    /// Two live captures of a claude row at rest, scrubbed, one second apart: a background agent's footer below the box ticks (`3m 9s` to `3m 11s`); every other row is identical.
    fn footer_frame(text: &str) -> Vec<String> {
        text.lines().filter(|l| !l.starts_with("cols=")).map(String::from).collect()
    }

    /// REAL: Claude Code 2.1.288, agent view off, one background sub-agent, 80x24 (2026-10-03), scrubbed. The screen
    /// reader that took them trims U+00A0 (`str::trim_end`); the wake's reader keeps the prompt's, so with `nbsp` a row
    /// that is the bare `❯` gets it back. The trailer gives the cursor.
    fn cc288(text: &str, nbsp: bool) -> (Vec<String>, (u16, u16)) {
        let mut lines = Vec::new();
        let mut cursor = None;
        for l in text.lines() {
            if let Some(rest) = l.strip_prefix("cols=") {
                let at = rest.split_whitespace().find_map(|w| w.strip_prefix("cursor=")).expect("cursor= in the trailer");
                let (r, c) = at.split_once(',').expect("row,col");
                cursor = Some((r.parse().unwrap(), c.parse().unwrap()));
            } else if nbsp && l == "\u{276f}" {
                lines.push("\u{276f}\u{a0}".to_string());
            } else {
                lines.push(l.to_string());
            }
        }
        (lines, cursor.expect("a trailer"))
    }

    #[test]
    fn cc288_agent_view_off_rest_reads_free() {
        let rests = [
            include_str!("../tests/fixtures/comm_wake/cc288-avoff-1-rest.txt"),
            include_str!("../tests/fixtures/comm_wake/cc288-avoff-4-esc.txt"),
            include_str!("../tests/fixtures/comm_wake/cc288-avoff-5-esc2.txt"),
        ];
        for text in rests {
            let (l, cur) = cc288(text, true);
            assert_eq!((l.len(), cur), (24, (15, 2)));
            for windows in [false, true] {
                assert_eq!(refused_on(&l, Some(cur), "claude", windows), None);
            }
        }
        // DERIVED from the f6 rest capture, with the footer of the live M4 failure on rc9.12 (agent view on, 2.1.288).
        let (mut l, cur) = cc288(rests[0], true);
        l[19] = "  ⏵⏵ auto mode on (shift+tab to cycle) · /tasks to see subagents · ← 1 agent".to_string();
        for windows in [false, true] {
            assert_eq!(refused_on(&l, Some(cur), "claude", windows), None);
        }
    }

    #[test]
    fn a_pane_too_narrow_for_the_wake_line_is_refused() {
        let b = |w: usize| vec!["\u{2500}".repeat(w), "\u{276f}\u{a0}".to_string(), "\u{2500}".repeat(w)];
        let need = 2 + WAKE_LINE.chars().count() + 2;
        for windows in [false, true] {
            assert_eq!(refused_on(&b(need - 1), Some((1, 2)), "claude", windows), Some("pane too narrow for the wake line"));
            assert_eq!(refused_on(&b(need), Some((1, 2)), "claude", windows), None);
        }
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
        // the hold in `wake_if_free` protects it (itest `a_working_row_is_not_typed_into_until_it_rests`). The
        // queued-message hint read blank if it is dim (unverified); either way the hold protects the row.
        let mut turn = LINUX_TURN_A;
        turn[8] = "\u{276f}\u{a0}";
        assert!(on(&turn, (8, 2), false));
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
        let mut idle = LINUX_IDLE;
        idle[8] = "\u{276f}\u{a0}";
        assert!(on(&idle, (8, 2), true));
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

    /// REAL, 2026-10-02 journal, Claude Code 2.1.285, 83 columns, cursor 41;3: a named session's box.
    #[test]
    fn a_named_session_box_is_free() {
        let rule = |n: usize| "\u{2500}".repeat(n);
        let top = format!("{} named-session \u{2500}", rule(49));
        let prompt = "\u{276f}\u{a0}".to_string();
        for windows in [false, true] {
            assert!(prompt_free_on(&[top.clone(), prompt.clone(), rule(83)], Some((1, 2)), "claude", windows));
        }
        let glued = format!("{}label\u{2500}", rule(10)); // no spaces around the label: not a border
        let two = format!("{} a {} b \u{2500}", rule(10), rule(10)); // two labels: not a border
        let inner = format!("{} a\u{2500}b \u{2500}", rule(10)); // a label containing the rule glyph
        let wide = " a label as wide as the line ".to_string(); // no rule left on either side
        for bad in [glued, two, inner, wide] {
            assert!(!prompt_free_on(&[bad, prompt.clone(), rule(83)], Some((1, 2)), "claude", false));
        }
    }
    const P_MAIN: &str = "  ● main";
    const P_SUB: &str = "  ◯ general-purpose  background task                    13s · ↓ 41.0k tokens";
    const F_LEADER: &str = "  ⏵⏵ auto mode on · 1 shell · ← for agents";
    const F_DOWN: &str = "  ⏵⏵ auto mode on · 1 shell";
    const F_SELECT: &str = "  ↑/↓ to select";
    const F_VIEW: &str = "  Enter to view · x to stop";
    /// The cursor in the probe frames' prompt row, where Claude Code leaves it in an empty prompt.
    const AT: (u16, u16) = (16, 2);

    fn rule80() -> String {
        "\u{2500}".repeat(80)
    }

    /// REAL layout, scrubbed: the 2026-10-02 probe captures (Claude Code 2.1.287, 80 columns, one background
    /// agent). Rows 15-17 are the box, 18-19 the statusline, 20 the footer, 22-23 the agents panel.
    fn probe(top: &str, prompt: &str, footer: &str, panel: [&str; 2]) -> Vec<String> {
        let rule = rule80();
        let mut f = vec![String::new(); 15];
        for l in [top, prompt, rule.as_str(), "  Opus 5.5 [00000000] acct ·think:xhigh | v2.1.287 | demo:main", "  Session: 51k (in:51k out:1k) | $0.00", footer, "", panel[0], panel[1], ""] {
            f.push(l.to_string());
        }
        f
    }

    /// The stuck-wake record's shape: a 4x80 screen, the prompt between two rules, the cursor at column 3 (1-based).
    fn parsed(prompt: &str) -> vt100_ctt::Parser {
        let rule = "\u{2500}".repeat(80);
        let mut p = vt100_ctt::Parser::new(4, 80, 0);
        p.process(format!("{rule}\r\n{prompt}\r\n{rule}\x1b[2;3H").as_bytes());
        p
    }

    #[test]
    fn the_leader_view_reads_free() {
        // Capture 10, and the same box with a named session's label in its top border.
        let named = format!("{} named-session \u{2500}", "\u{2500}".repeat(64));
        for top in [rule80(), named] {
            for windows in [false, true] {
                assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}", F_LEADER, [P_MAIN, P_SUB]), Some(AT), "claude", windows), None);
            }
        }
    }

    #[test]
    fn typed_into_main_table() {
        const L: &str = "[sot-comm] you have mail: run comm-poll.sh";
        let at = |col: u16| Some((16, col));
        let typed = |top: &str, prompt: &str, footer: &str, panel: [&str; 2], cur, windows| typed_refusal(&probe(top, prompt, footer, panel), cur, "claude", windows, L).is_none();
        let line = format!("\u{276f}\u{a0}{L}");
        // The line in a leader box, wherever the cursor sits on the prompt row.
        assert!(typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], at(2 + L.len() as u16), false));
        assert!(typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], at(2), false));
        assert!(typed_refusal(&boxed(&line), Some((1, 44)), "claude", false, L).is_none());
        // A focused panel under the box.
        assert!(!typed(&rule80(), &line, F_SELECT, [P_MAIN, "❯ ◯ general-purpose"], at(2), false));
        // The cursor on a panel row, off the box.
        assert!(!typed(&rule80(), &line, F_LEADER, [P_MAIN, P_SUB], Some((22, 0)), false));
        // The box empty, the line plus more, other text, no NBSP.
        assert!(!typed(&rule80(), "\u{276f}\u{a0}", F_LEADER, [P_MAIN, P_SUB], at(2), false));
        assert!(!typed(&rule80(), &format!("{line} and more"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
        assert!(!typed(&rule80(), &format!("\u{276f}\u{a0}hello {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
        assert!(!typed(&rule80(), &format!("\u{276f} {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), false));
        // A sub-agent's labelled box, its panel focused on it.
        let top = format!("{} background task \u{2500}", "\u{2500}".repeat(62));
        let viewing = ["  ◯ main", "❯ ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
        assert!(!typed(&top, &line, F_SELECT, viewing, at(2), false));
        // Windows: either glyph; the NBSP or, while it is not required there, a space.
        for glyph in ["\u{276f}", ">"] {
            assert!(typed(&rule80(), &format!("{glyph}\u{a0}{L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
            assert_eq!(typed(&rule80(), &format!("{glyph} {L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true), !nbsp_required(true));
            assert!(!typed(&rule80(), &format!("{glyph}{L}"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
            assert!(!typed(&rule80(), &format!("{glyph}\u{a0}{L} x"), F_LEADER, [P_MAIN, P_SUB], at(2), true));
        }
        // The line followed by Claude Code's dim suggestion: dim cells read blank, so the line still sits alone.
        let rule = "\u{2500}".repeat(80);
        let mut p = vt100_ctt::Parser::new(3, 80, 0);
        p.process(format!("{rule}\r\n\u{276f}\u{a0}{L}\x1b[2m try this\x1b[22m\r\n{rule}\x1b[2;{}H", 3 + L.len()).as_bytes());
        let seen = free_test_lines(p.screen());
        assert_eq!(typed_refusal(&seen, Some(p.screen().cursor_position()), "claude", false, L), None);
    }

    #[test]
    fn a_view_of_another_agent_is_refused() {
        // Capture 08: the agent's task in the top border, the dot and the panel cursor on the agent. The
        // placeholder `Message @general-purpose…` as the wake reads it if dim (blank), and as plain text.
        let top = format!("{} background task \u{2500}", "\u{2500}".repeat(62));
        let viewing = ["  ◯ main", "❯ ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
        assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}", F_SELECT, viewing), Some(AT), "claude", false), Some("agents panel focused or unrecognised"));
        assert_eq!(refused_on(&probe(&top, "\u{276f}\u{a0}Message @general-purpose…", F_SELECT, viewing), Some(AT), "claude", false), Some("input not empty"));
    }

    #[test]
    fn the_dot_on_another_agent_is_refused() {
        // DERIVED from capture 08 with no panel cursor, under the leader footer.
        let dot_on_sub = ["  ◯ main", "  ● general-purpose  background task                    13s · ↓ 41.0k tokens"];
        assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_LEADER, dot_on_sub), Some(AT), "claude", false), Some("the panel's dot is not on main"));
    }

    #[test]
    fn panel_focus_is_refused() {
        // Captures 06 and 09 (the panel cursor on main) and 07 (on the agent): an Enter there opens a view.
        let on_sub = "❯ ◯ general-purpose  background task                    13s · ↓ 41.0k tokens";
        for (footer, panel) in [(F_SELECT, ["❯ ● main", P_SUB]), (F_VIEW, [P_MAIN, on_sub])] {
            assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", footer, panel), Some(AT), "claude", false), Some("agents panel focused or unrecognised"));
        }
    }

    #[test]
    fn an_unknown_panel_layout_is_refused() {
        // DERIVED: a panel with no line for main, and a dot on a name other than main.
        for panel in [[P_SUB, ""], ["  ● team-lead", P_SUB]] {
            assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_LEADER, panel), Some(AT), "claude", false), Some("the panel's dot is not on main"));
        }
    }

    #[test]
    fn a_draft_reads_not_free_wherever_its_cursor_sits() {
        // At Home the cursor sits where an empty prompt's does (col 2); only the text after the NBSP tells.
        for windows in [false, true] {
            assert_eq!(refused_on(&boxed("\u{276f}\u{a0}hello"), Some((1, 2)), "claude", windows), Some("input not empty"));
            assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}hello", F_LEADER, [P_MAIN, P_SUB]), Some(AT), "claude", windows), Some("input not empty"));
        }
    }

    #[test]
    fn a_dim_suggestion_reads_empty_and_a_draft_does_not() {
        let p = parsed("\u{276f}\u{a0}\x1b[2mtry this\x1b[22m");
        let seen = free_test_lines(p.screen());
        assert_eq!(seen[1], "\u{276f}\u{a0}");
        assert_eq!(refused_on(&seen, Some(p.screen().cursor_position()), "claude", false), None);
        let p = parsed("\u{276f}\u{a0}try this");
        let seen = free_test_lines(p.screen());
        assert_eq!(seen[1], "\u{276f}\u{a0}try this");
        assert_eq!(refused_on(&seen, Some(p.screen().cursor_position()), "claude", false), Some("input not empty"));
        // Only the cursor's row is read this way: a dim line elsewhere keeps its text.
        let mut p = parsed("\u{276f}\u{a0}");
        p.process(b"\x1b[4;1H\x1b[2mfooter\x1b[22m\x1b[2;3H");
        assert_eq!(free_test_lines(p.screen())[3], "footer");
    }
    /// A log sink for one test: `tracing` writes here while the test's subscriber is the default.
    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_refusal_streak_logs_one_line_and_a_wake_or_a_read_ends_it() {
        let buf = LogBuf::default();
        let sink = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || sink.clone()).with_ansi(false).finish();
        let text = || String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let count = || text().matches("keeps refusing the wake").count();
        let refused = || Step::Refused(Refusal { reason: "input not empty", border: "──── named-session ─".to_string(), detail: None });
        let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
        let t0 = Instant::now();
        let mut at = |d: Duration, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + d);
        tracing::subscriber::with_default(sub, || {
            // Inside the bound: silent. Fails if the bound is ignored.
            at(Duration::ZERO, refused());
            at(REFUSED_FOR - Duration::from_secs(1), refused());
            assert_eq!(count(), 0, "inside the bound: silent");
            // One line per streak, with its fields. Fails if the info! is deleted, a field dropped, or a streak logs twice.
            at(REFUSED_FOR, refused());
            at(REFUSED_FOR * 5, refused());
            assert_eq!(count(), 1, "one line per streak");
            let t = text();
            assert!(t.contains("handle=h ") && t.contains("reason=\"input not empty\"") && t.contains("border=\"──── named-session ─\""), "{t}");
            // A wake ends the streak. Fails if it does not (the old streak is logged, so the count stays 1).
            at(REFUSED_FOR * 6, Step::Woke(Woken { line: 1, at: t0 }));
            at(REFUSED_FOR * 7, refused());
            assert_eq!(count(), 1, "a new streak waits its own bound");
            at(REFUSED_FOR * 8, refused());
            assert_eq!(count(), 2);
            // A read inbox ends it too. Fails if Clear leaves the streak.
            at(REFUSED_FOR * 9, Step::Clear);
            at(REFUSED_FOR * 10, refused());
            at(REFUSED_FOR * 11, refused());
            assert_eq!(count(), 3);
        });
    }

    #[test]
    fn cc288_agent_view_off_panel_focus_is_refused() {
        for text in [
            include_str!("../tests/fixtures/comm_wake/cc288-avoff-2-down1.txt"),
            include_str!("../tests/fixtures/comm_wake/cc288-avoff-3-down2.txt"),
        ] {
            let (l, cur) = cc288(text, true);
            assert_eq!(cur, (23, 0));
            assert_eq!(refused_on(&l, Some(cur), "claude", false), Some("not in an input box"));
            // The cursor put back on the prompt: the panel cursor alone refuses.
            assert_eq!(refused_on(&l, Some((15, 2)), "claude", false), Some("agents panel focused or unrecognised"));
        }
    }

    #[test]
    fn cc288_the_ticking_panel_is_not_held() {
        for (a, b) in [
            (include_str!("../tests/fixtures/comm_wake/cc288-avoff-1-rest.txt"), include_str!("../tests/fixtures/comm_wake/cc288-avoff-5-esc2.txt")),
        ] {
            let (rest, _) = cc288(a, true);
            let (later, _) = cc288(b, true);
            assert_ne!(rest, later);
            assert_eq!(held_rows(&rest, 15), held_rows(&later, 15));
        }
    }

    #[test]
    fn agent_view_focus_that_draws_nothing_reads_free() {
        // Capture 05 (2.1.287, agent view ON, the first ↓): focus left the input and nothing shows it. The named gap
        // of `panel_refusal`; agent view stays on in rows open at install until /reauth.
        assert_eq!(refused_on(&probe(&rule80(), "\u{276f}\u{a0}", F_DOWN, [P_MAIN, P_SUB]), Some(AT), "claude", false), None);
        assert_eq!(typed_refusal(&probe(&rule80(), &format!("\u{276f}\u{a0}{WAKE_LINE}"), F_DOWN, [P_MAIN, P_SUB]), Some(AT), "claude", false, WAKE_LINE), None);
    }

    #[test]
    fn an_attach_failure_skips_the_row() {
        let out: Result<WakeOutcome, HeadlessError> = Err(HeadlessError { phase: "attach", detail: "no supervisor".into(), submitted: false });
        assert!(matches!(step_of("h", out, (None, String::new()), 1, Instant::now()), Step::Skip));
    }

    #[test]
    fn a_typed_line_counts_as_the_wake_and_a_failed_write_is_tried_again() {
        let now = Instant::now();
        let seen = || (None, "b".to_string());
        let typed_no_enter = WakeOutcome::TypedNoEnter { reason: "typed text not in main's input box", border: String::new() };
        assert!(matches!(step_of("h", Ok(typed_no_enter), seen(), 7, now), Step::Woke(Woken { line: 7, .. })));
        let enter = WakeOutcome::Unconfirmed { step: "enter", detail: "record: input delivery unknown".into() };
        assert!(matches!(step_of("h", Ok(enter), seen(), 7, now), Step::Woke(Woken { line: 7, .. })));
        let text = WakeOutcome::Unconfirmed { step: "text", detail: "record: input delivery unknown".into() };
        match step_of("h", Ok(text), seen(), 7, now) {
            Step::Refused(r) => {
                assert_eq!(r.reason, "text not confirmed");
                assert_eq!(r.detail.as_deref(), Some("record: input delivery unknown"));
            }
            _ => panic!("a failed text write is a refusal"),
        }
    }

    #[test]
    fn a_wake_line_with_more_or_less_is_a_draft() {
        for text in [format!("{WAKE_LINE} and more"), WAKE_LINE[..WAKE_LINE.len() - 5].to_string()] {
            let held = format!("\u{276f}\u{a0}{text}");
            assert_eq!(refused_on(&boxed(&held), Some((1, 2)), "claude", false), Some("input not empty"));
            assert!(typed_refusal(&boxed(&held), Some((1, 2)), "claude", false, WAKE_LINE).is_some());
        }
    }

    #[test]
    fn an_unconfirmed_write_warns_once_per_streak() {
        let buf = LogBuf::default();
        let sink = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || sink.clone()).with_ansi(false).finish();
        let count = || String::from_utf8(buf.0.lock().unwrap().clone()).unwrap().matches("text not confirmed (record: input delivery unknown)").count();
        let u = || Step::Refused(Refusal { reason: "text not confirmed", border: "b".to_string(), detail: Some("record: input delivery unknown".to_string()) });
        let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
        let t0 = Instant::now();
        let mut at = |secs: u64, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + Duration::from_secs(secs));
        tracing::subscriber::with_default(sub, || {
            for secs in [0, 2, 4] {
                at(secs, u());
            }
            assert_eq!(count(), 1);
            at(6, Step::Woke(Woken { line: 1, at: t0 }));
            at(8, u());
            assert_eq!(count(), 2);
        });
    }
}
