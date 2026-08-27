//! The Windows capsule: one process babysitting one producer under an
//! owned ConPTY, writing its voyage (ADR 0041 step 4 — the capsule
//! runtime; ADR 0039 is the format it writes; `conpty.rs` is the
//! primitives layer this module orchestrates).
//!
//! Mirrors `capsule.rs`'s shape and spirit deliberately — same
//! writer-loop-owns-everything design, same frame protocol, same group-
//! commit/echo watermark, same input WAL shape — but is NOT that module:
//! a completely separate, independently maintained copy for a completely
//! different OS primitive (owned ConPTY + job containment vs. a bare PTY
//! fd). Even the parts that read identically (the base64 encoder, the
//! frame-context helper) are duplicated here, not shared: `capsule.rs` is
//! `#![cfg(target_os = "linux")]`-gated, so there is no common home for a
//! shared module without inventing one, which is out of this unit's scope.
//!
//! Three properties this module adds over the Linux capsule, all pinned by
//! ADR 0041 "Step 4 as specified":
//! - a live `vt100_ctt` parser tracks the producer's screen for a later
//!   attach (step 5) to checkpoint from — this unit only keeps it current
//!   and resizes it, it never serializes it (no attach lane exists yet);
//! - the ConPTY host-facing DA1 handshake (`host_handshake.rs`) is answered on every
//!   output chunk, including through the teardown drain, so the pre-24H2
//!   blocking close this sequence exists to survive can never deadlock on
//!   an unanswered query;
//! - spawn failure and BOTH teardown entry points (an externally requested
//!   kill, or the producer exiting on its own) are handled by ONE
//!   compensation path / ONE orchestrator, so a segment is always sealed,
//!   never abandoned — the Linux capsule's own known gap (a bare `?` on
//!   `spawn_on_pty` that escapes unsealed) is deliberately NOT inherited.
//!
//! Judgment calls made without an explicit ruling in the spec gate,
//! flagged here (and again in the unit's report) rather than treated as
//! settled:
//! - **DSR recorded as `control_exchange`.** The ADR states plainly that
//!   "the DSR reply frame `responds_to` its request" — so a request +
//!   response pair is emitted every time [`crate::host_handshake::HostHandshake::feed`]
//!   produces a reply. What is NOT pinned: which `source.actor` records
//!   it (this module uses [`FrameCtx::capsule_frame`], reasoning that
//!   answering a host query is autonomous capsule machinery — the same
//!   category as `producer_spawn`/`producer_dead` — never a driver
//!   action, so `controller_frame` would misattribute it), whether a
//!   third `outcome` phase belongs alongside request/response (not
//!   implemented — the reply IS the completion of a purely local decision
//!   already made, with nothing external left to report; resize NEEDS a
//!   later outcome only because acting on it can fail after the request
//!   is written, which has no analogue here), and what `to.kind` names
//!   (this module writes `{"kind": "producer"}`, reusing resize's own
//!   choice to mean "concerns the pty/producer channel", since no
//!   `ActorKind` names "the ConPTY host itself").
//! - **`ExitKind`** (below) has no ADR-pinned vocabulary — it exists only
//!   for the Rust caller (the ADR itself: "exit codes play no role in run
//!   lifetime", and the same is true of this signal, which never reaches
//!   the wire).
//! - **Resize's outcome `target`** names the resize REQUEST's own `seq`
//!   (`"{epoch}:{n}"`) — the closest analogue to claude.rs's interrupt
//!   precedent (which targets the TURN being closed), since a resize has
//!   no turn-like entity of its own to target.

#![cfg(windows)]

use crate::conpty::{observe_spawning_process_jobbed, ConptySpawn};
use crate::host_handshake::HostHandshake;
use crate::envelope::*;
use crate::segment::{Commit, RetentionClass};
use crate::voyage::VoyageStore;
use crate::{Error, Result};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(50);
const GROUP_COMMIT_BYTES: usize = 256 * 1024;
const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const READ_CHUNK: usize = 8192;
const LOCAL_CONTROLLER: &str = "local";

/// ADR 0041 "Terminal state" resource budget — the geometry a resize (or
/// the initial spawn) may request. Independent from the vt100 fork's own
/// `grid::MIN_ROWS`/`MIN_COLS` and `checkpoint::MAX_ROWS`/`MAX_COLS`
/// (confirmed identical by reading the fork's source — all four are
/// `pub(crate)` there, unreachable from here) — pinned separately so this
/// module's enforcement doesn't silently drift if the fork's own private
/// constants ever do; a mismatch would surface loudly, as
/// `Screen::checkpoint`/`set_size` refusing a geometry this module thought
/// was in-budget, never silently.
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_COLS: u16 = 512;
const MAX_ROWS: u16 = 256;

/// The output channel's byte budget (ADR 0041 budget table: "producer
/// channel 8 MiB bounded"). Bounds OUTSTANDING bytes only — chunks the
/// reader thread has read from ConPTY but the writer loop has not yet
/// committed — never a second buffer of its own; see [`OutputBudget`].
const OUTPUT_QUEUE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

/// Bounded wait for the containment job to reap every in-job process after
/// `TerminateJobObject` (teardown step 2). Generous because it covers an
/// entire process TREE under load, not a single wait; a real failure to
/// reap is a genuine bug or an unkillable hang, and either way deserves a
/// loud, diagnosable error rather than an indefinite one.
const TEARDOWN_REAP_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_REAP_POLL: Duration = Duration::from_millis(20);

/// Bounded wait, during teardown, for the reader thread's own terminal
/// sentinel (`Event::ReaderEof`) after `close_pty()` — pre-24H2's
/// documented blocking close is exactly what this waits through (the
/// reader is already draining concurrently, satisfying the documented call
/// pattern); a timeout here means the close itself never returned, which
/// is a real, diagnosable failure (see `Pseudoconsole::close_pty`'s doc).
const TEARDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const TEARDOWN_DRAIN_POLL: Duration = Duration::from_millis(200);

pub struct CapsuleWinConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    pub producer_kind: String,
    /// argv[0] is the program; must be non-empty (`ConptySpawn::spawn`'s
    /// own check is what actually enforces this).
    pub argv: Vec<String>,
    /// Echo producer output to the capsule's stdout after commit — same
    /// visibility watermark as the Linux capsule: after the fsync that
    /// covers it, never before.
    pub echo: bool,
    /// Initial terminal geometry, validated by the SAME 2x2..512x256 rule
    /// a later resize is (ADR 0041: "Initial geometry is validated by the
    /// same rule").
    pub cols: u16,
    pub rows: u16,
}

/// The driver-facing command surface this unit exposes. ADR 0041's attach
/// lane's `resize` op and the mgmt lane's `shutdown` (which drives
/// `EndRun`) are both step 5's job to WIRE onto a real named pipe — this is
/// the internal Rust channel step 5 forwards into, not the wire protocol
/// itself, and this unit does not implement admission (who may call it).
#[derive(Debug, Clone, Copy)]
pub enum ControlCmd {
    /// "resize {cols, rows} — driver-only" (ADR 0041 attach protocol);
    /// this unit only implements the ordered exchange once a command
    /// arrives.
    Resize { cols: u16, rows: u16 },
    /// The step-4-visible primitive behind `EndRun` (ADR 0041 Lifecycle):
    /// an EXTERNALLY REQUESTED end. Never inferred from an exit code, a
    /// channel disconnect, or anything else — only an explicit `Kill`.
    Kill,
}

/// Why `run` returned. In-memory only — no frame field encodes this (ADR
/// 0041 Lifecycle: "Exit codes play no role in run lifetime", and the same
/// is true of this signal, which exists purely for the Rust caller). Naming
/// is a judgment call: no ADR-pinned vocabulary exists for it yet (EndRun's
/// own reason enum is step 5's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// The producer exited on its own; this run's own program ending is
    /// what closed it — never treated as a request.
    ProducerExited,
    /// `ControlCmd::Kill` was received.
    Requested,
    /// The producer's ConPTY output pipe reached EOF before this loop ever
    /// called `close_pty()`. Believed unreachable in normal operation
    /// (`conpty.rs`'s own contract: ConPTY keeps `hOutput` open regardless
    /// of child lifetime until explicitly closed) — handled defensively
    /// rather than assumed impossible, matching this codebase's own
    /// observation-not-assumption stance elsewhere.
    ReaderClosedUnexpectedly,
    /// `ConptySpawn::spawn` failed, or the initial geometry was outside
    /// the budget — nothing ever ran. `producer_dead {spawn_failed:true}`
    /// was still committed and the segment still sealed.
    SpawnFailed,
}

#[derive(Debug)]
pub struct ExitSummary {
    pub exit_code: Option<i32>,
    pub exit_kind: ExitKind,
    pub frames_written: u64,
    pub segments_sealed: u64,
}

/// One event the writer loop services. Mirrors `capsule.rs`'s `Event`
/// (`Output`/`Input`/`ProducerEof`), extended with the driver command
/// surface: `Control` bridges the public `control` parameter in, and
/// `ReaderEof` is the reader thread's own terminal sentinel — needed for
/// exactly the reason `capsule.rs`'s `ProducerEof` is: this channel ALSO
/// carries input and control, so the channel's own aggregate disconnect
/// can't be relied on to mean "the producer is gone"; an explicit last
/// message can.
enum Event {
    Output(Vec<u8>),
    Input(Vec<u8>),
    Control(ControlCmd),
    ReaderEof,
}

/// 16 random bytes from the OS, as lowercase hex32 (the ADR idem_key
/// shape) — duplicated from `capsule.rs` rather than shared; see the
/// module doc.
fn random_idem_key() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(std::io::Error::from)?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The bounded output budget (ADR 0041: "producer channel 8 MiB bounded —
/// when full the writer loop stops POLLING output... control/liveness
/// always serviced"). Implemented as a shared OUTSTANDING-byte counter
/// rather than a second queue structure: `Event::Output` chunks still ride
/// the one shared channel every other event does, but the READER THREAD
/// blocks (via this condvar) before enqueueing more once the budget is
/// exhausted — so Input/Control, sent by different threads, are never
/// gated by it, and the writer loop never has two structures to poll. The
/// reader thread stalling before its next `read()` is exactly how
/// backpressure "lands in ConPTY": nothing drains `hOutput`, so ConPTY's
/// own internal buffer is what fills next.
struct OutputBudget {
    outstanding: Mutex<u64>,
    space_available: Condvar,
}

impl OutputBudget {
    fn new() -> Self {
        Self {
            outstanding: Mutex::new(0),
            space_available: Condvar::new(),
        }
    }

    /// Reader-thread-only: blocks while the budget is exhausted, then
    /// reserves `n` bytes against it.
    fn reserve(&self, n: u64) {
        let mut outstanding = self.outstanding.lock().unwrap();
        while *outstanding >= OUTPUT_QUEUE_BUDGET_BYTES {
            outstanding = self.space_available.wait(outstanding).unwrap();
        }
        *outstanding += n;
    }

    /// Writer-loop-only: releases `n` bytes once the frame that carried
    /// them has been appended (accounted for — not necessarily fsynced;
    /// this budget bounds OUTSTANDING work, not durability), waking any
    /// reader thread blocked in `reserve`.
    fn release(&self, n: u64) {
        let mut outstanding = self.outstanding.lock().unwrap();
        *outstanding = outstanding.saturating_sub(n);
        self.space_available.notify_one();
    }
}

/// The writer loop's frame factory: sequential seq, capsule clocks, and the
/// per-run refs (attached_to / input WAL) threaded through one small state.
/// Identical in shape to `capsule.rs`'s `FrameCtx` — duplicated, not
/// shared (module doc).
struct FrameCtx {
    epoch: u64,
    next_n: u64,
    t0: Instant,
    take_epoch: u64,
    attached: Option<Seq>,
}

impl FrameCtx {
    fn seq(&mut self) -> Seq {
        let s = Seq {
            epoch: self.epoch,
            n: self.next_n,
        };
        self.next_n += 1;
        s
    }
    fn mono_us(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }
    fn capsule_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        Envelope {
            seq: self.seq(),
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Unknown,
                    controller_id: None,
                    take_epoch: None,
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: wall_ms(),
            t_mono_us: self.mono_us(),
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(payload),
            payload_ref: None,
        }
    }
    fn controller_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some(LOCAL_CONTROLLER.into()),
            take_epoch: Some(self.take_epoch),
        };
        e
    }
    fn producer_frame(&mut self, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(Class::Producer, payload);
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        e.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: self.attached.expect("attached before producer frames"),
        }];
        e
    }
}

/// Run one producer under a Windows capsule. Blocks until the run ends —
/// either the producer exits on its own, or `control` delivers
/// [`ControlCmd::Kill`] (ADR 0041 Lifecycle: an externally requested end).
/// `control` mirrors `claude.rs`'s `operator: mpsc::Receiver<OperatorCmd>`
/// parameter — the raw command surface a future attach lane (step 5)
/// forwards into.
// unused_assignments: `flush_output!`'s state reset is dead only at its
// FINAL expansion (after the loop) — load-bearing at every other site,
// same allow capsule.rs carries for the identical reason.
#[allow(unused_assignments)]
pub fn run(config: CapsuleWinConfig, control: mpsc::Receiver<ControlCmd>) -> Result<ExitSummary> {
    // Resolve ONCE — see capsule.rs's identical comment on the same call.
    let voyage_root = crate::fsutil::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    let mut store = VoyageStore::open_for_writing(&voyage_root, &config.voyage_id)?;
    store.seal_survivor()?;

    let mut ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        attached: None,
    };
    let mut w = store.open_segment(wall_ms())?;
    let mut seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let mut segments_sealed: u64 = 0;

    // Control preamble — every frame here commits immediately. Identical
    // shape to capsule.rs: revoke-first (null holder, bumped epoch), then
    // grant local.
    let prior_take = store.last_take_epoch;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": prior_take + 1, "holder": null}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;
    ctx.take_epoch = prior_take + 2;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": ctx.take_epoch, "holder": LOCAL_CONTROLLER}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // producer_attached: the raw-terminal redaction profile, content-hashed
    // — identical to capsule.rs (this is a cross-platform semantic, not a
    // Linux one).
    let rules = json!({"input_content": "redacted", "turns": "none"});
    let rules_bytes = serde_json::to_vec(&rules)?;
    let rules_sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&rules_bytes);
        h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()
    };
    let attached_seq_holder = ctx.capsule_frame(
        Class::ProducerAttached,
        json!({
            "producer_kind": config.producer_kind,
            "version": "raw-pty-1",
            "profile_def": {"id": "raw-terminal-default", "sha256": rules_sha, "rules": rules},
        }),
    );
    let attached_seq = attached_seq_holder.seq;
    w.append(&attached_seq_holder, Commit::Immediate)?;
    frames_written += 1;
    ctx.attached = Some(attached_seq);

    // producer_spawn commits BEFORE spawn is even attempted — the
    // spawn-failure compensation path (below) depends on this already
    // being on the wire. `spawning_process_was_jobbed` is observed here,
    // independent of spawn's own outcome (see `observe_spawning_process_jobbed`'s
    // doc) — `SpawnDetail`, only available after a SUCCESSFUL spawn, would
    // be too late for a failure to fold in.
    let spawning_process_was_jobbed = observe_spawning_process_jobbed();
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_spawn",
               "detail": {"argv": config.argv, "spawning_process_was_jobbed": spawning_process_was_jobbed}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // Initial geometry validated by the SAME rule a resize is (ADR 0041).
    // An out-of-budget request here is treated exactly like a spawn
    // failure: nothing was ever created, so the same compensation path
    // applies — no separate code path needed for "never even tried".
    let geometry_ok =
        (MIN_COLS..=MAX_COLS).contains(&config.cols) && (MIN_ROWS..=MAX_ROWS).contains(&config.rows);
    let spawn_result = if !geometry_ok {
        Err(format!(
            "initial geometry {}x{} outside the 2x2..512x256 budget",
            config.cols, config.rows
        ))
    } else {
        ConptySpawn::spawn(&config.argv, config.cols, config.rows).map_err(|e| e.to_string())
    };

    let spawn = match spawn_result {
        Ok(s) => s,
        Err(reason) => {
            // Compensation: the producer never ran, but a real spawn
            // attempt was already recorded above — close the run out
            // honestly instead of the Linux capsule's known bare-`?`
            // escape (which seals nothing). No pty/job exists to tear
            // down; this is the one path that reaches producer_dead
            // without ever having a ConptySpawn.
            let f = ctx.capsule_frame(
                Class::Lifecycle,
                json!({"kind": "producer_dead",
                       "detail": {"exit_code": null, "spawn_failed": true, "reason": reason}}),
            );
            w.append(&f, Commit::Immediate)?;
            frames_written += 1;
            let digest = w.seal(None)?;
            store.advance_chain(digest);
            segments_sealed += 1;
            return Ok(ExitSummary {
                exit_code: None,
                exit_kind: ExitKind::SpawnFailed,
                frames_written,
                segments_sealed,
            });
        }
    };

    // Destructure rather than keep `spawn` around: partial moves out of a
    // Drop-less struct (conpty.rs pins `ConptySpawn` as deliberately
    // Drop-less) are fine, and this avoids repeating `spawn.` everywhere.
    // `pid`/`detail` are dropped: `detail` duplicates the pre-spawn
    // jobbed-observation already committed above (see conpty.rs's own
    // doc — the value is identical either way), and `pid` has no use in
    // this unit (nothing here re-opens the process by PID).
    let ConptySpawn {
        job,
        process,
        pty,
        reader,
        mut writer,
        pid: _,
        detail: _,
    } = spawn;

    // Live vt100 parser (ADR 0041 "Terminal state") — kept current for a
    // later attach (step 5) to checkpoint from; this unit never serializes
    // it. NO scrollback (the capsule keeps none by design).
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, 0);
    let mut handshake = HostHandshake::new();

    // Reader thread + stdin thread + control-bridge thread, all funneling
    // into one channel (mirrors capsule.rs's own reader+stdin-thread
    // design, extended with the control bridge).
    let (tx, rx) = mpsc::channel::<Event>();
    let output_budget = Arc::new(OutputBudget::new());
    let reader_handle = {
        let tx = tx.clone();
        let budget = Arc::clone(&output_budget);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    // Believed unreachable before `close_pty()` runs (see
                    // `ExitKind::ReaderClosedUnexpectedly`'s doc) — handled
                    // as ordinary loop termination regardless, never a
                    // panic, since "should never happen" is not "cannot".
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        budget.reserve(n as u64);
                        if tx.send(Event::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Event::ReaderEof);
        })
    };
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; READ_CHUNK];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(Event::Input(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = control.recv() {
                if tx.send(Event::Control(cmd)).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let mut stdout = std::io::stdout();
    let mut pending_echo: Vec<u8> = Vec::new();
    let mut pending_bytes: usize = 0;
    let mut last_commit = Instant::now();

    macro_rules! flush_output {
        ($w:expr) => {
            if pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is echoed
                if config.echo {
                    let _ = stdout.write_all(&pending_echo);
                    let _ = stdout.flush();
                }
                pending_echo.clear();
                pending_bytes = 0;
            }
            last_commit = Instant::now();
        };
    }

    macro_rules! maybe_rotate {
        ($w:ident) => {
            if seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                store.advance_chain(digest);
                segments_sealed += 1;
                $w = store.open_segment(wall_ms())?;
                seg_bytes = 0;
            }
        };
    }

    // One producer-output handler, used identically pre-teardown AND
    // during the teardown drain — so "the handshake keeps answering
    // through the drain" (ADR 0041) can't be missed by one call site and
    // not the other. Feeds the live parser, answers any DA1 query,
    // records the raw producer frame, and tracks the group-commit/echo
    // state.
    macro_rules! handle_output {
        ($bytes:expr) => {{
            let bytes = $bytes;
            parser.process(&bytes);

            use base64_engine::encode_b64;
            let f = ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
            w.append(&f, Commit::Buffered)?;
            frames_written += 1;
            seg_bytes += bytes.len() as u64 + 128;
            output_budget.release(bytes.len() as u64);
            pending_echo.extend_from_slice(&bytes);
            pending_bytes += bytes.len();
            if pending_bytes >= GROUP_COMMIT_BYTES {
                flush_output!(w);
            }

            // The host-facing handshake (see this module's doc for the
            // recording judgment call): reply written to the pty's input
            // regardless of admission state, request+response recorded as
            // a control_exchange pair.
            let reply = handshake.feed(&bytes);
            if !reply.is_empty() {
                let req = ctx.capsule_frame(
                    Class::ControlExchange,
                    json!({"phase": "request", "kind_ns": "conpty/host-handshake",
                           "to": {"kind": "producer"}, "body": {"query": "da1"}}),
                );
                let req_seq = req.seq;
                w.append(&req, Commit::Immediate)?;
                frames_written += 1;
                // Requested-then-performed, honestly: the response frame
                // commits ONLY when the reply bytes actually reached the
                // pty's input. A failed write — likely during the teardown
                // drain, when the pty is closing under us — leaves the
                // request standing alone, which the record reads correctly
                // as "query observed, never answered". Committing the
                // response on a discarded write result would be the exact
                // requested-vs-performed drift the resize exchange exists
                // to prevent.
                if writer.write_all(&reply).is_ok() {
                    let mut resp = ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "response", "kind_ns": "conpty/host-handshake",
                               "body": {"query": "da1", "reply": String::from_utf8_lossy(&reply).into_owned()}}),
                    );
                    resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                    w.append(&resp, Commit::Immediate)?;
                    frames_written += 1;
                }
            }
        }};
    }

    // Main loop: natural-exit polled every iteration (bounded to one
    // GROUP_COMMIT_WINDOW of latency, regardless of event volume), then
    // one event serviced per iteration.
    let exit_kind = 'main: loop {
        if process.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        match rx.recv_timeout(GROUP_COMMIT_WINDOW) {
            Ok(Event::Output(bytes)) => {
                handle_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(Event::Input(bytes)) => {
                // WAL order identical to capsule.rs: input -> intent ->
                // syscall -> forwarded.
                flush_output!(w);
                let input = ctx.controller_frame(
                    Class::Input,
                    json!({"idem_key": random_idem_key()?, "content": "redacted", "length": bytes.len()}),
                );
                let input_seq = input.seq;
                w.append(&input, Commit::Immediate)?;
                let mut intent = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forward_intent"}}),
                );
                intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                let intent_seq = intent.seq;
                w.append(&intent, Commit::Immediate)?;
                writer.write_all(&bytes)?; // the forward syscall
                let mut fwd = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forwarded",
                                     "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
                );
                fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                w.append(&fwd, Commit::Immediate)?;
                frames_written += 3;
                maybe_rotate!(w);
            }
            Ok(Event::Control(ControlCmd::Resize { cols, rows })) => {
                // ADR 0041: "Resize rejects, never clamps" — an
                // ordered-writer-loop command: request committed -> one
                // ResizePseudoConsole call (skipped entirely if
                // out-of-budget) -> parser + geometry updated only on
                // success -> outcome committed. request+outcome only (no
                // response phase): the exchange resolves entirely within
                // this loop, nothing external to await.
                flush_output!(w);
                let req = ctx.controller_frame(
                    Class::ControlExchange,
                    json!({"phase": "request", "kind_ns": "conpty/resize",
                           "to": {"kind": "producer"}, "body": {"cols": cols, "rows": rows}}),
                );
                let req_seq = req.seq;
                w.append(&req, Commit::Immediate)?;
                frames_written += 1;

                let in_budget =
                    (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
                let outcome_body = if !in_budget {
                    json!({"disposition": "failed", "cols": cols, "rows": rows,
                           "reason": "outside the 2x2..512x256 budget"})
                } else {
                    match pty.resize(cols, rows) {
                        Ok(()) => {
                            // vt100_ctt's own argument order is (rows,
                            // cols) — the opposite of ConPTY's (cols,
                            // rows); both call sites are spelled out
                            // explicitly rather than sharing a tuple, so a
                            // future reorder of one can't silently swap
                            // the other.
                            parser.screen_mut().set_size(rows, cols);
                            json!({"disposition": "ok", "cols": cols, "rows": rows})
                        }
                        Err(e) => json!({"disposition": "failed", "cols": cols, "rows": rows,
                                          "reason": e.to_string()}),
                    }
                };
                let out = ctx.controller_frame(
                    Class::ControlExchange,
                    json!({"phase": "outcome", "kind_ns": "conpty/resize", "scope": "pty",
                           "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                );
                w.append(&out, Commit::Immediate)?;
                frames_written += 1;
                maybe_rotate!(w);
            }
            Ok(Event::Control(ControlCmd::Kill)) => break 'main ExitKind::Requested,
            Ok(Event::ReaderEof) => break 'main ExitKind::ReaderClosedUnexpectedly,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_commit.elapsed() >= GROUP_COMMIT_WINDOW {
                    flush_output!(w);
                }
            }
            // Only reachable if every sender is gone, which (since the
            // stdin/control threads' sends never themselves fail while
            // their own upstream is live) means the reader thread ended —
            // the same anomaly `Event::ReaderEof` names explicitly. Folded
            // into the same teardown rather than left to spin: recv_timeout
            // returns Disconnected immediately, not after the timeout, so
            // ignoring it here would busy-loop.
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'main ExitKind::ReaderClosedUnexpectedly,
        }
    };
    flush_output!(w);

    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for every exit_kind above — natural exit and a requested kill both
    // land here, and every step is unconditional: terminating an already-
    // empty job, or closing an already-idle pty, is harmless.
    //
    // Step 1: terminate the job. Forces a still-running tree down for a
    // requested kill; a harmless no-op if the producer already exited on
    // its own (nothing left in the job to terminate).
    job.terminate()?;

    // Step 2: bounded reap poll — "reaps the tree" (containment covers
    // every in-job descendant, not just the primary).
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        if job.active_processes()? == 0 {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(Error::State(
                "capsule_win: job did not reap within the teardown timeout".into(),
            ));
        }
        std::thread::sleep(TEARDOWN_REAP_POLL);
    }

    // Step 3: close the pseudoconsole. May block pre-24H2 — the reader
    // thread is ALREADY running and draining concurrently (spawned back
    // at producer_spawn time and never stopped), which is exactly the
    // documented call pattern this depends on.
    pty.close_pty();

    // Step 4: keep committing output (and answering the handshake) through
    // the close, until the reader's own terminal sentinel arrives — this
    // is where "producer_dead + seal happen only after reader EOF" (ADR
    // 0041) is satisfied. Admission is revoked here (Input/Control are
    // still received on the shared channel but simply dropped, never
    // processed) — this unit has no epoch/admission model to make a
    // richer refusal fact meaningful; that belongs to step 5's attach
    // lane, layered above this channel.
    // If the reader's EOF sentinel was ALREADY consumed (the
    // ReaderClosedUnexpectedly path), waiting for a second one would wedge
    // this loop until the timeout errors — a defensive branch that ends in
    // a guaranteed stall defeats its purpose. Drain whatever is queued and
    // let the first empty poll end the drain instead.
    let reader_already_eof = matches!(exit_kind, ExitKind::ReaderClosedUnexpectedly);
    let drain_deadline = Instant::now() + TEARDOWN_DRAIN_TIMEOUT;
    loop {
        match rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
            Ok(Event::Output(bytes)) => {
                handle_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(Event::Input(_)) | Ok(Event::Control(_)) => {} // admission revoked
            Ok(Event::ReaderEof) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if reader_already_eof {
                    break; // nothing further can arrive; queue drained
                }
                if Instant::now() >= drain_deadline {
                    return Err(Error::State(
                        "capsule_win: reader did not reach EOF within the teardown drain timeout".into(),
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    flush_output!(w);
    let _ = reader_handle.join();

    // Step 5: the primary's own exit status. `wait()` first establishes
    // the honesty-bound precondition `PrimaryProcess::exit_code`'s own doc
    // requires (STILL_ACTIVE disambiguation) — ActiveProcesses==0 above
    // already proved the process isn't running, but this satisfies the
    // bound by the letter of its doc, not just by inference.
    if !process.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: process handle did not signal after the job reaped to zero".into(),
        ));
    }
    let exit_code = match process.exit_code()? {
        Some(c) => Some(c as i32),
        None => {
            return Err(Error::State(
                "capsule_win: process reported STILL_ACTIVE after the job was fully reaped".into(),
            ));
        }
    };

    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_dead", "detail": {"exit_code": exit_code}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    let digest = w.seal(None)?;
    store.advance_chain(digest);
    segments_sealed += 1;

    Ok(ExitSummary {
        exit_code,
        exit_kind,
        frames_written,
        segments_sealed,
    })
}

/// Minimal base64 (standard alphabet, padded) — duplicated from
/// `capsule.rs`; see the module doc.
mod base64_engine {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode_b64(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
        }
        out
    }
}
