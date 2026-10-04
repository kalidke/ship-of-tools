//! The daemon's own headless attach client on a capsule lane: type into a row, read its screen.

use std::path::Path;
use sot_protocol::PtyEnter;
use std::time::{Duration, Instant};

use sot_log::client::PlatformEndpoint;
use sot_log::fe_client::TAKE_QUEUE_CAP;
use sot_log::fe_client_io::{FeAttachClient, InputOutcome};
use sot_log::state_dir::state_dir_hash;

/// This daemon build's own concrete attach client — always the real
/// platform endpoint (pipes on Windows, a Unix socket on Linux). No
/// caller of this module ever needs to name `E` itself.
pub(crate) type Client = FeAttachClient<PlatformEndpoint>;

/// How often the daemon polls its own headless client. Independent of
/// (and much finer than) the deadline a caller passes in — this is
/// just the local spin-wait granularity, not a protocol budget.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Bound for the client's own worker-thread join on every exit path
/// (`FeAttachClient::shutdown`). Generous relative to the worker's
/// 100 ms tick, but still a REAL bound — "on EVERY exit path: drop the
/// client, then observe the worker's closure" (ADR 0042 amendment §3).
pub(crate) const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

/// What a headless op failed to do, and where. `phase` is one of
/// `size` (the size gate, before any attach), `attach`, `checkpoint`,
/// `take` (the pen was asked for but never granted, and — since
/// nothing reached [`Client::send_input`]'s own wire flush yet —
/// `submitted` is `false` here), `input` (a definite
/// `input_refused_stale`; never retried by this module), `record`
/// (the record's own verdict is UNKNOWABLE: either the wire said
/// `input_delivery_unknown`, or the deadline expired after the input
/// had already been handed to the lane — both mean the same thing to
/// a caller: do not retry, and do not assume failure either), or
/// `detach` (reserved for the shutdown bound itself; see its own doc —
/// today this is only ever logged, never returned as an `Err`).
#[derive(Debug)]
pub struct HeadlessError {
    pub phase: &'static str,
    pub detail: String,
    /// `true` iff the input had already been handed to the lane
    /// (`Client::send_input` called) when the failure/expiry hit —
    /// the daemon's own signal to answer `capsule_input_unknown`
    /// rather than a flat failure (ADR 0042 amendment §2).
    pub submitted: bool,
}

/// `screen_of`'s own result: the row's current, visible screen.
/// `cursor` is `(row, col)`, matching `vt100_ctt::Screen::
/// cursor_position`'s own order.
pub struct ScreenShot {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<String>,
    pub cursor: Option<(u16, u16)>,
}

/// Types `bytes` into the row at `state_dir` as `controller_id`,
/// taking the pen only long enough to deliver them — never resizing
/// the pane (a headless client has no viewport to size it to). One
/// absolute `deadline` covers attach, checkpoint, take, and the wait
/// for the wire's own verdict on the input; every exit path drops the
/// client and observes the worker's closure within [`SHUTDOWN_WAIT`].
/// Returns the number of payload bytes delivered (never counting a
/// trailing Enter byte the caller may have already folded in — this
/// function has no opinion on that, it delivers exactly what it is
/// given).
pub fn type_into(
    state_dir: &Path,
    controller_id: &str,
    bytes: &[u8],
    deadline: Instant,
) -> Result<usize, HeadlessError> {
    if bytes.len() > TAKE_QUEUE_CAP {
        return Err(HeadlessError {
            phase: "size",
            detail: format!(
                "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                bytes.len()
            ),
            submitted: false,
        });
    }
    if bytes.is_empty() {
        // "an empty payload succeeds without taking" (ADR 0042
        // amendment review) — nothing to attach for.
        return Ok(0);
    }

    let mut client = attach(state_dir, controller_id)?;
    if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
        client.shutdown(SHUTDOWN_WAIT);
        return Err(e);
    }
    let result = send_and_wait_recorded(&mut client, bytes, deadline);
    client.shutdown(SHUTDOWN_WAIT);
    result
}

/// The send/wait-for-verdict loop, factored out of [`type_into`] so
/// [`write_and_enter`] can call it twice (text, then Enter) on ONE
/// continuous attach — a mid-sequence pen loss then surfaces as an
/// ordinary `RefusedStale`/`is_dead()` on the same client.
fn send_and_wait_recorded(client: &mut Client, bytes: &[u8], deadline: Instant) -> Result<usize, HeadlessError> {
    let expected = bytes.len() as u64;
    let before = client.recorded_bytes();
    client.send_input(bytes);
    loop {
        client.pump();
        if let Some(outcome) = client.last_input_outcome() {
            match outcome {
                InputOutcome::Recorded => {
                    if client.recorded_bytes().saturating_sub(before) >= expected {
                        return Ok(bytes.len());
                    }
                    // A single `send_input` call is always flushed as
                    // ONE input frame in practice (the payload already
                    // fits under `TAKE_QUEUE_CAP`, so nothing splits
                    // it) — this branch should be unreachable, but
                    // correctness does not depend on that: keep
                    // polling for the rest, bounded by the same
                    // deadline, rather than declaring victory early.
                    if Instant::now() >= deadline {
                        return Err(HeadlessError {
                            phase: "record",
                            detail: "deadline exceeded before the whole payload was recorded"
                                .to_string(),
                            submitted: true,
                        });
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                InputOutcome::RefusedStale => {
                    return Err(HeadlessError {
                        phase: "input",
                        detail: "input refused as stale (the take epoch changed); \
                                 this op is never retried"
                            .to_string(),
                        submitted: true,
                    });
                }
                InputOutcome::DeliveryUnknown => {
                    return Err(HeadlessError {
                        phase: "record",
                        detail: "input delivery unknown".to_string(),
                        submitted: true,
                    });
                }
            }
            continue;
        }
        if client.is_dead() {
            return Err(HeadlessError {
                phase: "take",
                detail: client.status_line().to_string(),
                submitted: true,
            });
        }
        if Instant::now() >= deadline {
            return Err(HeadlessError {
                phase: "record",
                detail: "deadline exceeded waiting for the input to be recorded".to_string(),
                submitted: true,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// A capsule row's SPLIT `enter: true` write: text, a bounded PACING
/// wait, then Enter as its own write — makes NO submission claim (a
/// screen snapshot cannot prove one) and never retries either write.
///
/// 1. [`type_into`]'s own size gate runs first, before any attach.
/// 2. Writes the text — a hard error, never retried, on any failure.
/// 3. PACES only (claims nothing): waits for the screen to hold still
///    `quiet_budget`, bounded overall at `pacing_budget`.
/// 4. Writes Enter under the text's own grant (headless never
///    re-takes): a pen change in between is refused stale by the
///    supervisor, so `enter: not_sent`. Once the text is recorded
///    (step 2), this ALWAYS answers `Ok`: an Enter failure is never a
///    hard `Err` (a retried caller could double the text); it is
///    `not_sent` for the stale refusal and `unknown` for every other
///    failure (see [`enter_outcome`]).
/// 5. Never retries: the worker's auto-retake after a stale refusal
///    is disabled for headless transactions.
///
/// RESIDUAL RISK: this never checks whether a human already has an
/// unsent draft here (parity with the old tmux path, which never did
/// either) — a real check needs attach-proto v3's pen-holder signal
/// (`PenSnapshot`/`holder`), left to the Stage 2 resident-attach lanes.
///
/// Returns `(bytes_written, enter)`: `enter` is `sent` only when the
/// Enter byte was written and recorded, never a claim that codex
/// treated it as a submitted turn; `not_sent` when the supervisor
/// refused it; `unknown` otherwise.
pub fn write_and_enter(
    state_dir: &Path,
    controller_id: &str,
    text: &[u8],
    op_budget: Duration,
    quiet_budget: Duration,
    pacing_budget: Duration,
) -> Result<(usize, PtyEnter), HeadlessError> {
    if text.len() > TAKE_QUEUE_CAP {
        return Err(HeadlessError {
            phase: "size",
            detail: format!(
                "payload is {} bytes, exceeding the take queue cap of {TAKE_QUEUE_CAP} bytes",
                text.len()
            ),
            submitted: false,
        });
    }

    let mut client = attach(state_dir, controller_id)?;
    if let Err(e) = wait_for_checkpoint(&mut client, Instant::now() + op_budget) {
        client.shutdown(SHUTDOWN_WAIT);
        return Err(e);
    }
    let out = type_and_pace(&mut client, text, op_budget, quiet_budget, pacing_budget)
        .map(|n| (n, enter_outcome(send_enter(&mut client, op_budget))));
    client.shutdown(SHUTDOWN_WAIT);
    out
}

/// What the Enter write's result says to a `pty.input` caller: only the stale refusal (phase `"input"`, the
/// one outcome where nothing was written, see `send_and_wait_recorded`) is `NotSent`; every other failure may
/// have reached the agent.
pub(crate) fn enter_outcome(r: Result<(), HeadlessError>) -> PtyEnter {
    match r {
        Ok(()) => PtyEnter::Sent,
        Err(e) if e.phase == "input" => PtyEnter::NotSent,
        Err(_) => PtyEnter::Unknown,
    }
}

/// [`write_and_enter`]'s steps 2-3 (type, then wait for the screen to settle) over an already attached,
/// checkpointed client; the caller shuts the client down.
pub(crate) fn type_and_pace(
    client: &mut Client,
    text: &[u8],
    op_budget: Duration,
    quiet_budget: Duration,
    pacing_budget: Duration,
) -> Result<usize, HeadlessError> {
    let n = if text.is_empty() {
        0
    } else {
        match send_and_wait_recorded(client, text, Instant::now() + op_budget) {
            Ok(n) => n,
            Err(e) => return Err(e),
        }
    };
    // `SOT_TEST_PACING_HOLD` (test-only, the `SOT_TEST_ACTIVATION_BARRIER`
    // convention): hold pacing to its full bound. Terminal output batches,
    // so a scripted test load cannot keep the screen changing every poll.
    let pacing_hold = std::env::var_os("SOT_TEST_PACING_HOLD").is_some();

    let pacing_deadline = Instant::now() + pacing_budget;
    let mut previous = current_lines(client);
    let mut last_change_at = Instant::now();
    loop {
        let quiet_elapsed = !pacing_hold && Instant::now().duration_since(last_change_at) >= quiet_budget;
        if quiet_elapsed || Instant::now() >= pacing_deadline {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
        client.pump();
        let lines = current_lines(client);
        if lines != previous {
            previous = lines;
            last_change_at = Instant::now();
        }
    }

    Ok(n)
}

/// [`write_and_enter`]'s step 4: the Enter byte, written and recorded. Doc above.
pub(crate) fn send_enter(client: &mut Client, op_budget: Duration) -> Result<(), HeadlessError> {
    send_and_wait_recorded(client, &[0x0d], Instant::now() + op_budget).map(|_| ())
}

/// Current screen lines, top to bottom, trailing spaces trimmed —
/// [`screen_of`]'s own shape, off an already-pumped client.
fn current_lines(client: &Client) -> Vec<String> {
    let (_, cols) = client.screen().size();
    client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect()
}

/// Reads the current, visible screen of the row at `state_dir` as a
/// pure WATCHER — never takes the pen, never sends input. Same
/// deadline/shutdown discipline as [`type_into`].
pub fn screen_of(
    state_dir: &Path,
    controller_id: &str,
    deadline: Instant,
) -> Result<ScreenShot, HeadlessError> {
    let mut client = attach(state_dir, controller_id)?;
    if let Err(e) = wait_for_checkpoint(&mut client, deadline) {
        client.shutdown(SHUTDOWN_WAIT);
        return Err(e);
    }
    let (rows, cols) = client.screen().size();
    let lines: Vec<String> =
        client.screen().rows(0, cols).map(|line| line.trim_end().to_string()).collect();
    let cursor = Some(client.screen().cursor_position());
    client.shutdown(SHUTDOWN_WAIT);
    Ok(ScreenShot { cols, rows, lines, cursor })
}

pub(crate) fn attach(state_dir: &Path, controller_id: &str) -> Result<Client, HeadlessError> {
    Client::attach_headless(PlatformEndpoint::default(), state_dir_hash(state_dir), controller_id.to_string()).map_err(|e| {
        HeadlessError { phase: "attach", detail: e.to_string(), submitted: false }
    })
}

pub(crate) fn wait_for_checkpoint(client: &mut Client, deadline: Instant) -> Result<(), HeadlessError> {
    loop {
        client.pump();
        if client.is_checkpointed() {
            return Ok(());
        }
        if client.is_dead() {
            return Err(HeadlessError {
                phase: "checkpoint",
                detail: client.status_line().to_string(),
                submitted: false,
            });
        }
        if Instant::now() >= deadline {
            return Err(HeadlessError {
                phase: "checkpoint",
                detail: "deadline exceeded before a checkpoint arrived".to_string(),
                submitted: false,
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod headless_size_gate_tests {
    // Pure size-gate tests: `type_into` checks the payload length BEFORE
    // ever attempting an attach, so these need no supervisor, no state
    // dir on disk, and no real process at all — a nonexistent path is
    // fine, and a real attach attempt against it would prove the test
    // wrong (the size gate must short-circuit before that).
    use super::{enter_outcome, type_into, write_and_enter, HeadlessError};
    use sot_protocol::PtyEnter;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn enter_outcome_tells_unknown_from_not_sent() {
        let stale = HeadlessError { phase: "input", detail: "stale".into(), submitted: true };
        let unknown = HeadlessError { phase: "record", detail: "unknown".into(), submitted: true };
        let take = HeadlessError { phase: "take", detail: "dead".into(), submitted: true };
        assert_eq!(enter_outcome(Ok(())), PtyEnter::Sent);
        assert_eq!(enter_outcome(Err(stale)), PtyEnter::NotSent);
        assert_eq!(enter_outcome(Err(unknown)), PtyEnter::Unknown);
        assert_eq!(enter_outcome(Err(take)), PtyEnter::Unknown);
    }

    #[test]
    fn oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
    }

    #[test]
    fn exactly_the_cap_is_not_oversized() {
        // The cap itself is legal — only `CAP + 1` is refused. This
        // would attempt a real attach (and fail on the nonexistent path
        // some other way), which is enough to prove the size gate did
        // NOT reject it — that failure is expected and not asserted on
        // further than "it is not the size-gate error."
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP];
        let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
            .expect_err("a nonexistent state dir cannot succeed");
        assert_ne!(err.phase, "size", "the cap itself must not trip the size gate");
    }

    #[test]
    fn empty_payload_succeeds_with_no_attach() {
        let n = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &[], deadline())
            .expect("an empty payload succeeds trivially, with nothing to attach for");
        assert_eq!(n, 0);
    }

    #[test]
    fn write_and_enter_oversized_payload_is_refused_before_any_attach() {
        let bytes = vec![b'x'; sot_log::fe_client::TAKE_QUEUE_CAP + 1];
        let budget = Duration::from_secs(5);
        let err = write_and_enter(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, budget, budget, budget)
            .expect_err("oversized payload must be refused");
        assert_eq!(err.phase, "size");
        assert!(!err.submitted);
    }
}
