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
//! (`screen::free_test_lines`): Claude Code draws its suggestion and placeholders dim, so they read empty,
//! while a typed draft is not dim and reads not free wherever its cursor sits. The input must reach the main
//! agent: no agents panel below the box, or a panel with no panel cursor and its one dot on main
//! ([`panel_refusal`]); Claude Code's footer is not read. A pane too narrow for the wake line on the prompt's one
//! row refuses ([`fits`]). A row with unread mail that the wake refuses for [`REFUSED_FOR`] gets one log line per run of
//! refusals, naming its handle, the reason and the line above the prompt. Known limits: a suggestion drawn by colour rather than dim reads as a draft, and so does every
//! suggestion on Windows until a screen read shows SGR 2 survives ConPTY; a statusline that draws `●` or `◯`
//! below the box refuses the row; with no panel nothing below the box is checked, so a view of another agent
//! or a focus off the input that drew no panel would read free (every captured view draws the panel); with agent view on, a
//! focus on the panel that draws nothing reads free ([`panel_refusal`]).
//! Enter goes only after a screen read shows the typed line alone in main's input box ([`typed_refusal`]), which the
//! wake waits up to [`OP_BUDGET`] for after typing; otherwise no Enter goes, the
//! attempt counts as the wake (within one daemon run the line is not typed again before [`REPEAT_AFTER`]) and owes its Enter, and a warning names
//! it. A later tick ([`Decision::Complete`]) sends Enter alone, never typing, once the first frame and the live screen after the hold both show just the wake line in main's box.
//! That gate withholds Enter after a stray key between the
//! final read and the typing; the line itself has then gone, without Enter, wherever that key put focus (the
//! panel or a draft). Nothing guards the window between the gate's read and the Enter: a key pressed, or a dialog
//! or permission prompt drawn, in it receives the Enter. A later wake refuses for whatever the screen then
//! shows, and the refusal streak logs it.
//! The owed Enter outlives the mail: a line left in main's box is the wake's to finish however the mail is read (the
//! Stop hook, or `comm-poll.sh` by hand), and the agent then runs `comm-poll.sh` and finds nothing new. While mail is
//! unread the debt lasts until [`REPEAT_AFTER`]; once it is read, until that Enter goes or main's box reads empty
//! [`REPEAT_AFTER`] or more after the typing ([`line_gone`]; sooner, an empty box may be an echo still to come). A box
//! that holds anything but just the line gets nothing and is looked at again every tick, as a row with unread mail is.
//! Known gap (Windows): the typed line has failed to read alone in main's box within [`OP_BUDGET`] of the typing, though
//! a later read of the same row matched it; no Windows screen of a box holding typed text has been captured, so whether
//! the echo came late or something else shared the row is not known. A later tick sends the Enter once the row rests
//! with just the line in the box.
//!
//! A text write that fails after it may have landed (any phase but a stale `input` refusal) is treated as typed: the
//! line is owed its Enter and is not typed again, so a write that was lost for good costs the row its wake until
//! [`REPEAT_AFTER`], read inbox or not (the Complete bound, [`line_gone`]). An Enter refused as stale (another controller took the pen
//! after the typing) wrote nothing, so the line still owes it; so does an Enter whose delivery is unknown, since one
//! that landed leaves the box empty, which a Complete tick never sends Enter to.
//! Enter alone also goes to a box that holds exactly the wake line for another reason: a line recalled from history with Up or returned to the box by Esc, once it has held still for
//! [`STILL_FOR`]. The check cannot tell it from a line the wake left (it never reads the cursor column).
//!
//! "Last woken" lives in the tick task's memory, never on disk, so a daemon
//! restart wakes every row with unread mail once, at its first free prompt, and forgets an owed Enter. So a row with
//! its mail read gets one boot look on its row's first Ready tick of a run ([`check_row`]): a box that holds exactly the
//! wake line ([`Prompt::HoldsLine`]) is sent Enter alone, as a Complete tick does; any other box is left alone and the
//! row is not attached for this reason again. A row not yet Ready (the daemon re-adopts its rows at boot) or that cannot
//! be read keeps the look pending, and says so once in the log; a screen that is not a free prompt (a dialog, a draft)
//! or a `stop_at` mark under a minute old ends the look, and says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::rows::run::headless::HeadlessError;
use crate::rows::Workspaces;

mod attempt;
pub mod dialog;
mod screen;
mod unread;

use attempt::{wake_if_free, WakeOutcome};
use screen::{prompt_glyphs, prompt_of, Prompt};
use unread::scan;

pub const TICK: Duration = Duration::from_secs(2);
/// Mail still unread this long after a wake gets one more line.
const REPEAT_AFTER: Duration = Duration::from_secs(600);
/// A row with unread mail, or owed an Enter, that the wake has refused this long is logged, once per run of refusals,
/// so a prompt the free test does not recognise is not silent.
const REFUSED_FOR: Duration = Duration::from_secs(60);
/// A Complete tick's refusal when main's box is empty: the line has not drawn yet, or has gone.
const LINE_NOT_SHOWN: &str = "the typed wake line has not shown";
/// The Stop hook's longest run (its auditor call is capped at 45 s; Codex kills the hook at 10 s). A row whose
/// registry entry carries a `stop_at` mark at most this old is running its Stop hook and is not typed into; an
/// older mark is a Stop that never ended (Esc, an API error, a killed hook).
const STOP_HOOK_BOUND: Duration = Duration::from_secs(60);
const WAKE_LINE: &str = "[sot-comm] you have mail: run comm-poll.sh";
const CONTROLLER_ID: &str = "sot-comm-wake";
const OP_BUDGET: Duration = Duration::from_secs(3);
/// A turn whose only change is a once-a-second counter must change inside the hold even when the screen reaches the daemon ~100 ms late.
const STILL_FOR: Duration = Duration::from_millis(1500);

/// When a handle was last woken: the inbox's complete-line count then.
#[derive(Debug, Clone, Copy)]
struct Woken {
    line: u64,
    at: Instant,
    /// The line was typed and no Enter went: a later tick may send Enter alone ([`Decision::Complete`]), never type again.
    enter_owed: bool,
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
    let iso = crate::comm::registry::registry::iso8601_utc_from_secs;
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
    /// The line typed and Enter owed, and the mail read or [`REPEAT_AFTER`] not yet passed: only Enter alone may be sent.
    Complete,
}

fn decide(s: &Scan, woken: Option<&Woken>, now: Instant) -> Decision {
    // An owed Enter outlives the mail: a line left in main's box stays there however the mail is read.
    if woken.is_some_and(|w| w.enter_owed && (s.unread == 0 || now.duration_since(w.at) < REPEAT_AFTER)) {
        Decision::Complete
    } else if s.unread == 0 {
        Decision::Clear
    } else if s.fresh > 0 || woken.map_or(true, |w| now.duration_since(w.at) >= REPEAT_AFTER) {
        Decision::Wake
    } else {
        Decision::Hold
    }
}

/// The tick. Runs forever; started once from `server::run`.
pub async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let mut woken: HashMap<String, Woken> = HashMap::new();
    let mut streaks: HashMap<String, Streak> = HashMap::new();
    // Handles already given their one boot look ([`check_row`]), and those whose look is waiting on the row (logged once).
    let mut looked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut waiting: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let rows = workspaces.list();
        let mut checks = Vec::new();
        for ws in rows {
            let handle = ws.agent_handle();
            if handle.is_empty() || ws.runtime != "capsule" {
                continue;
            }
            let agent = ws.agent();
            if prompt_glyphs(&agent, cfg!(windows)).is_empty() {
                continue;
            }
            let state_dir = crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id);
            let home = comm_home.clone();
            let prior = woken.get(&handle).copied();
            let h = handle.clone();
            let first = !looked.contains(&handle);
            // Concurrent: each wake holds the screen for STILL_FOR, so N rows
            // with mail cost one hold per tick, not N.
            checks.push((handle, tokio::task::spawn_blocking(move || check_row(&home, &h, &state_dir, &agent, prior, first))));
        }
        for (handle, check) in checks {
            if let Ok(step) = check.await {
                match &step {
                    Step::Looked | Step::Woke(_) => {
                        looked.insert(handle.clone());
                    }
                    Step::LookPending(why) if waiting.insert(handle.clone()) => {
                        tracing::info!(handle = %handle, "comm wake: boot look waits: {why}");
                    }
                    _ => {}
                }
                settle(&mut woken, &mut streaks, handle, step, Instant::now());
            }
        }
    }
}

enum Step {
    Clear,
    Skip,
    /// The boot look is done, however it ended: forget the row as [`Step::Clear`] does, and look no more.
    Looked,
    /// The boot look could not be made (the row is not Ready, or could not be read): try again next tick.
    LookPending(&'static str),
    Woke(Woken),
    /// The screen was not a free prompt, or did not hold still.
    Refused(Refusal),
}

/// Why the wake did not type into a row with mail: a [`prompt_of`] reason, `stop hook running`, `moved during the
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
        Step::Clear | Step::Looked => {
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
                tracing::info!(handle = %handle, reason = ?r.reason, border = ?r.border, "comm wake: a row with unread mail or an unsent wake line keeps refusing the wake");
            }
        }
        Step::Skip | Step::LookPending(_) => {}
    }
}

/// `first` is the handle's first tick in this daemon run. A row with its mail read gets one boot look then, a Complete
/// tick that forgets the row afterwards however it ends: the daemon's memory of an owed Enter did not survive the
/// restart, so a wake line left typed in main's box is sent its Enter alone ([`Prompt::HoldsLine`]) and any other box
/// is left alone and not looked at again for this reason. While the row is not Ready or cannot be read the look stays
/// pending ([`Step::LookPending`]) and the next tick is `first` again; once it is made, [`Step::Looked`].
fn check_row(home: &Path, handle: &str, state_dir: &Path, agent: &str, prior: Option<Woken>, first: bool) -> Step {
    let s = scan(home, handle, prior.map_or(0, |w| w.line));
    let look = first && prior.is_none() && s.unread == 0;
    let decision = if look { Decision::Complete } else { decide(&s, prior.as_ref(), Instant::now()) };
    let complete = match decision {
        Decision::Clear => return Step::Clear,
        Decision::Hold => return Step::Skip,
        Decision::Wake => false,
        Decision::Complete => true,
    };
    // Only a Ready row is typed into: a row whose agent has ended can still
    // show a prompt-shaped last screen. (Nothing on the wake path restarts a
    // row; `wake_if_free` only attaches.)
    let ready = crate::rows::run::probe::phase_str(sot_log::lane::wire::SupervisorPhase::Ready);
    if crate::rows::run::probe::phase_of(state_dir) != ready {
        if look {
            return Step::LookPending("the row is not Ready");
        }
        return Step::Skip;
    }
    let seen: std::cell::RefCell<(Option<&'static str>, String)> = Default::default();
    let free = |l: &[String], c: Option<(u16, u16)>, a: &str| -> Option<Prompt> {
        // The registry, then the clock: a mark the read sees was stamped no later than `now`.
        let registry = crate::comm::registry::registry::read_registry_fresh(&home.join("registry.json")).unwrap_or_default();
        let now = crate::comm::registry::registry::unix_now_secs();
        let prompt = prompt_of(l, c, a, cfg!(windows));
        let reason = match prompt {
            Err(reason) => Some(reason),
            // A Complete tick sends Enter to a line already in the box; an empty box is not its business.
            Ok(Prompt::Empty) if complete => Some(LINE_NOT_SHOWN),
            Ok(_) => stop_hook_running(&registry, handle, now).then_some("stop hook running"),
        };
        let border = c.and_then(|(row, _)| l.get((row as usize).checked_sub(1)?)).cloned().unwrap_or_default();
        *seen.borrow_mut() = (reason, border);
        prompt.ok().filter(|_| reason.is_none())
    };
    let out = wake_if_free(state_dir, CONTROLLER_ID, WAKE_LINE, &free, agent, STILL_FOR, OP_BUDGET);
    let seen = seen.take();
    if look {
        match (&out, seen.0) {
            (Err(_), _) => return Step::LookPending("the row could not be read"),
            (Ok(_), Some(reason)) if reason != LINE_NOT_SHOWN => tracing::info!(handle, reason, "comm wake: boot look left main's box alone"),
            _ => {}
        }
        return Step::Looked;
    }
    if line_gone(&s, prior.as_ref(), seen.0, Instant::now()) {
        tracing::info!(handle, "comm wake: main's input box is empty and the mail is read; the unsent wake line is no longer owed its Enter");
        return Step::Clear;
    }
    step_of(handle, out, seen, s.total, Instant::now())
}

/// Whether a Complete tick that found main's box empty ([`LINE_NOT_SHOWN`]) ends the debt: the mail is read and
/// [`REPEAT_AFTER`] has passed since the typing, so the line has gone (sent or cleared by hand, or never drawn). Sooner,
/// an empty box may be an echo still to come; while mail is unread, [`decide`] bounds the debt.
fn line_gone(s: &Scan, prior: Option<&Woken>, reason: Option<&str>, now: Instant) -> bool {
    s.unread == 0 && reason == Some(LINE_NOT_SHOWN) && prior.is_some_and(|w| w.enter_owed && now.duration_since(w.at) >= REPEAT_AFTER)
}

/// What one wake attempt means for the row. A line that was typed counts as the wake whether or not Enter followed or
/// was confirmed (ADR 0049: one line per batch): typing it again would repeat it wherever focus went, or send it twice.
/// One typed without Enter is owed it (`enter_owed`): a later tick sends Enter alone once the line shows alone ([`Decision::Complete`]).
/// A text write refused as stale (phase `input`, nothing written) is a refusal, decided by the next tick's screen read
/// and warned once per streak ([`settle`]); any other text-write failure may have landed, so it counts as typed and
/// owes its Enter ([`WakeOutcome::TextUnknown`]). An Enter refused as stale wrote nothing, so it is owed
/// ([`WakeOutcome::EnterRefused`]), and so is one whose delivery is unknown: if it was lost the line is still in the box.
/// An attach or checkpoint failure is no row this tick.
fn step_of(handle: &str, out: Result<WakeOutcome, HeadlessError>, seen: (Option<&'static str>, String), total: u64, now: Instant) -> Step {
    let (reason, border) = seen;
    match out {
        Ok(WakeOutcome::Woke) => Step::Woke(Woken { line: total, at: now, enter_owed: false }),
        Ok(WakeOutcome::NotFree) => Step::Refused(Refusal { reason: reason.unwrap_or("moved during the hold"), border, detail: None }),
        Ok(WakeOutcome::TypedNoEnter { reason, border }) => {
            tracing::warn!(handle, border = ?border, "comm wake: typed the line but it did not show in main's input box ({reason}); no Enter sent");
            Step::Woke(Woken { line: total, at: now, enter_owed: true })
        }
        Ok(WakeOutcome::TextUnknown { detail }) => {
            tracing::warn!(handle, border = ?border, "comm wake: text not confirmed ({detail}); the line may have landed, so it is not typed again and its Enter is owed");
            Step::Woke(Woken { line: total, at: now, enter_owed: true })
        }
        Ok(WakeOutcome::EnterRefused { detail }) => {
            tracing::warn!(handle, border = ?border, "comm wake: Enter refused ({detail}); the line was typed, so it is not typed again and its Enter is owed");
            Step::Woke(Woken { line: total, at: now, enter_owed: true })
        }
        Ok(WakeOutcome::Unconfirmed { step: "enter", detail }) => {
            tracing::warn!(handle, border = ?border, "comm wake: enter not confirmed ({detail}); the line was typed, so it is not typed again and its Enter is owed");
            Step::Woke(Woken { line: total, at: now, enter_owed: true })
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

    #[test]
    fn batches() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(601);
        let w = Woken { line: 3, at: t0, enter_owed: false };
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
        // A typed line owed its Enter: only Enter alone, fresh mail or not, until REPEAT_AFTER; then a new line; read,
        // still only Enter alone (`an_owed_enter_outlives_the_mail`).
        let owed = Woken { enter_owed: true, ..w };
        assert_eq!(decide(&mail(3, 0), Some(&owed), t0), Decision::Complete);
        assert_eq!(decide(&mail(4, 1), Some(&owed), t0), Decision::Complete);
        assert_eq!(decide(&mail(4, 1), Some(&owed), later), Decision::Wake);
        assert_eq!(decide(&mail(0, 0), Some(&owed), t0), Decision::Complete);
    }

    /// A line typed and left unsent stays owed its Enter after its mail is read (by the Stop hook, or `comm-poll.sh` by
    /// hand), at any age, so a later tick still finishes it. Fails while a read inbox forgets the debt (Clear), which
    /// leaves the line in the prompt.
    #[test]
    fn an_owed_enter_outlives_the_mail() {
        let t0 = Instant::now();
        let owed = Woken { line: 3, at: t0, enter_owed: true };
        let read = Scan { total: 3, unread: 0, fresh: 0 };
        assert_eq!(decide(&read, Some(&owed), t0), Decision::Complete);
        assert_eq!(decide(&read, Some(&owed), t0 + REPEAT_AFTER * 3), Decision::Complete);
        assert_eq!(decide(&read, Some(&Woken { enter_owed: false, ..owed }), t0), Decision::Clear);
    }

    /// The debt ends on an empty box only with the mail read and REPEAT_AFTER past the typing; a box that holds the
    /// line, or anything else, keeps it. Fails if an empty box ends it sooner (a slow echo would then be left in the
    /// prompt) or never (an empty row would be attached to every tick for good).
    #[test]
    fn an_empty_box_ends_the_debt_only_with_the_mail_read_and_repeat_after_past() {
        let t0 = Instant::now();
        let later = t0 + REPEAT_AFTER;
        let owed = Woken { line: 3, at: t0, enter_owed: true };
        let (read, unread) = (Scan { total: 3, unread: 0, fresh: 0 }, Scan { total: 3, unread: 3, fresh: 0 });
        assert!(line_gone(&read, Some(&owed), Some(LINE_NOT_SHOWN), later));
        assert!(!line_gone(&read, Some(&owed), Some(LINE_NOT_SHOWN), later - Duration::from_secs(1)));
        assert!(!line_gone(&unread, Some(&owed), Some(LINE_NOT_SHOWN), later));
        for other in [None, Some("cursor not at the prompt"), Some("stop hook running")] {
            assert!(!line_gone(&read, Some(&owed), other, later), "{other:?}");
        }
        assert!(!line_gone(&read, Some(&Woken { enter_owed: false, ..owed }), Some(LINE_NOT_SHOWN), later));
        assert!(!line_gone(&read, None, Some(LINE_NOT_SHOWN), later));
    }

    #[test]
    fn a_stop_mark_holds_only_inside_the_bound() {
        let now = 1_800_000_000;
        let iso = crate::comm::registry::registry::iso8601_utc_from_secs;
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
    fn a_refusal_streak_logs_one_line_and_a_wake_or_a_read_ends_it() {
        let log = sot_log::test_log::capture();
        let text = || log.text();
        let count = || text().matches("keeps refusing the wake").count();
        let refused = || Step::Refused(Refusal { reason: "input not empty", border: "──── named-session ─".to_string(), detail: None });
        let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
        let t0 = Instant::now();
        let mut at = |d: Duration, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + d);
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
        at(REFUSED_FOR * 6, Step::Woke(Woken { line: 1, at: t0, enter_owed: false }));
        at(REFUSED_FOR * 7, refused());
        assert_eq!(count(), 1, "a new streak waits its own bound");
        at(REFUSED_FOR * 8, refused());
        assert_eq!(count(), 2);
        // A read inbox ends it too. Fails if Clear leaves the streak.
        at(REFUSED_FOR * 9, Step::Clear);
        at(REFUSED_FOR * 10, refused());
        at(REFUSED_FOR * 11, refused());
        assert_eq!(count(), 3);
    }

    #[test]
    fn an_attach_failure_skips_the_row() {
        let out: Result<WakeOutcome, HeadlessError> = Err(HeadlessError { phase: "attach", detail: "no supervisor".into(), submitted: false });
        assert!(matches!(step_of("h", out, (None, String::new()), 1, Instant::now()), Step::Skip));
    }

    /// The one text refusal left: `send_and_wait_recorded`'s phase `input`, in the wake's `phase: detail` shape.
    const STALE: &str = "input: input refused as stale (the take epoch changed); this op is never retried";

    #[test]
    fn a_typed_line_counts_as_the_wake_and_a_write_refused_as_stale_is_tried_again() {
        let now = Instant::now();
        let seen = || (None, "b".to_string());
        let typed_no_enter = WakeOutcome::TypedNoEnter { reason: "typed text not in main's input box", border: String::new() };
        assert!(matches!(step_of("h", Ok(typed_no_enter), seen(), 7, now), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
        // An Enter of unknown delivery may have been lost, leaving the line in the box: owed, so a later tick finishes it.
        let enter = WakeOutcome::Unconfirmed { step: "enter", detail: "record: input delivery unknown".into() };
        assert!(matches!(step_of("h", Ok(enter), seen(), 7, now), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
        let text = WakeOutcome::Unconfirmed { step: "text", detail: STALE.into() };
        match step_of("h", Ok(text), seen(), 7, now) {
            Step::Refused(r) => {
                assert_eq!(r.reason, "text not confirmed");
                assert_eq!(r.detail.as_deref(), Some(STALE));
            }
            _ => panic!("a text write refused as stale is a refusal"),
        }
    }

    /// An Enter the leg refused as stale (phase `input`) wrote nothing (`send_and_wait_recorded`), so the typed line
    /// still owes it: a later tick sends Enter alone once the box holds just the line. Fails while the stale Enter
    /// is booked as possibly delivered (`enter_owed: false`), which leaves the line unsent until REPEAT_AFTER.
    #[test]
    fn an_enter_refused_as_stale_is_still_owed() {
        let stale = HeadlessError { phase: "input", detail: "input refused as stale (the take epoch changed); this op is never retried".into(), submitted: true };
        let out = attempt::wake_outcome(Err(stale));
        let step = step_of("h", Ok(out), (None, "b".to_string()), 7, Instant::now());
        assert!(matches!(step, Step::Woke(Woken { line: 7, enter_owed: true, .. })), "an Enter refused as stale must stay owed");
    }

    #[test]
    fn a_text_write_of_unknown_delivery_owes_its_enter_and_is_not_typed_again() {
        let out = WakeOutcome::TextUnknown { detail: "record: input delivery unknown".into() };
        assert!(matches!(step_of("h", Ok(out), (None, "b".to_string()), 7, Instant::now()), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
    }

    #[test]
    fn an_unconfirmed_write_warns_once_per_streak() {
        let log = sot_log::test_log::capture();
        let count = || log.text().matches(&format!("text not confirmed ({STALE})")).count();
        let u = || Step::Refused(Refusal { reason: "text not confirmed", border: "b".to_string(), detail: Some(STALE.to_string()) });
        let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
        let t0 = Instant::now();
        let mut at = |secs: u64, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + Duration::from_secs(secs));
        for secs in [0, 2, 4] {
            at(secs, u());
        }
        assert_eq!(count(), 1);
        at(6, Step::Woke(Woken { line: 1, at: t0, enter_owed: false }));
        at(8, u());
        assert_eq!(count(), 2);
    }
}
