//! Ship's Log voyage store — frame codec + segment format.
//!
//! ADR 0039 is the normative contract; this crate implements it. The bytes
//! this crate writes are read forever, so behavior here favors refusing
//! loudly over guessing: only a *provably* torn tail is ever discarded, and
//! every other defect halts. Nothing is ever deleted (v1 has no GC, no
//! retention deletion, no forks, no packs — those return through the
//! `codec_id` / `required_features` / version seams).

pub mod attach_proto;
pub mod capsule;
pub mod capsule_win;
// ADR 0041 step 6, unit U0: the same-connection challenge's
// platform-neutral core (the outcome vocabulary, the connection trait,
// the wire half). L1-unix LU1a: ungated -- see the module's own doc.
pub mod challenge;
// L1-unix LU1a: the Windows half of the same-connection challenge --
// steps 1-3, the retained process-handle wrapper, and the raw-handle
// extension trait. `pub`, matching `pipe_win`/`capsule_win`: Windows-only,
// self-gated (see the module's own `#![cfg(windows)]`).
pub mod challenge_win;
// ADR 0041 step 6, unit U2: the probe classifier (Stage A/B transition
// table) `probe.rs` deliberately ships without — see that module's own
// doc. Portable (L1-unix LU1a): makes no OS call of its own, so its unit
// tests now run on every platform, not merely Windows.
pub mod classify;
pub mod claude;
pub mod conpty;
// L1-unix, unit LU0: the platform-neutral transport contract (hoisted out
// of `capsule_win`/`pipe_win`) -- deliberately NOT cfg-gated, unlike its
// siblings below: `pipe_win`/`pipe_transport` are the Windows
// implementation, a Unix implementation lands in LU1.
pub mod transport;
// ADR 0041 step 5, unit U3: the Windows named-pipe transport (server +
// client). `pub`, matching `conpty`/`capsule_win`/`wire`: its tests live in
// `tests/pipe_win.rs`, a separate integration-test crate that can only ever
// reach `pub` items — the same reason those sibling modules are `pub`
// rather than `pub(crate)`.
pub mod pipe_win;
// ADR 0041 step 5, unit U3 round 2: the thin bridge from `pipe_win`'s real
// named-pipe transport to `transport`'s `Transport` trait. Lives in the
// library (not the `sot-capsule` bin) for two reasons: `tests/e2e_pipe.rs`
// needs to reach it, and the bin needs nothing from it beyond construction
// -- one bridge, reused by both, rather than duplicated or made
// unreachable from the test crate.
pub mod pipe_transport;
// L1-unix LU1b (ADR 0043): the Unix domain-socket transport server --
// `SocketServer` (bind/accept, per-connection reader+writer threads, one
// bounded events channel, byte-budgeted outbound, two-phase teardown).
// `pub`, matching `pipe_win`: its tests live in `tests/socket_unix.rs`, a
// separate integration-test crate that can only ever reach `pub` items.
// Self-gated (`#![cfg(unix)]`), like `pipe_win` is self-gated to Windows.
pub mod socket_unix;
// L1-unix LU1b: the thin bridge from `socket_unix`'s real Unix-domain-
// socket transport to `transport`'s `Transport` trait -- the Unix twin of
// `pipe_transport`. Self-gated (`#![cfg(unix)]`).
pub mod socket_transport;
// ADR 0041 step 6, unit U0: fault-injection scaffolding for the probe
// classifier's own (later) model test. NO classifier logic lives here —
// see the module's own doc. Platform-neutral (L1-unix LU1a): the
// mechanical outcome enums, the `ProbeOps` trait, and the scripted test
// support -- the real OS-facing implementation is `probe_win`.
pub mod probe;
// L1-unix LU1a: the Windows half of the probe seam -- `RealProbeOps` and
// `SpawnedChild`. `pub`, matching `probe`/`challenge_win`: Windows-only,
// self-gated (see the module's own `#![cfg(windows)]`).
pub mod probe_win;
// Crate-private (Codex review finding, capsule_win.rs round): ADR 0041's
// "one private machine" ruling means this module's items are not part of
// the crate's public API — `capsule_win.rs` is the only real caller and
// reaches it via `crate::host_handshake::...`, which needs no `pub` beyond
// the crate boundary. Not `#[cfg(windows)]`: its own tests are pure bytes
// and run on every platform (see the module doc) — which is exactly why a
// plain (non-test) build on a non-Windows target now has NO caller at all
// for these now-private items (the only real caller, capsule_win.rs, is
// windows-only): `cfg_attr` suppresses the resulting dead_code warning
// there specifically, rather than losing it crate-wide or windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
mod host_handshake;
// ADR 0041 step 6, unit U0 round-1: the three-state deadline race
// `challenge::exchange_identity`'s bounded body uses. Portable -- no OS
// dependency at all -- so its own tests run everywhere, not merely on
// Windows. Crate-private (round-2 finding 7): no caller outside this
// crate needs it yet -- challenge.rs is a sibling module, not an
// external consumer. L1-unix LU1a: `exchange_identity` itself is now
// ungated, but ITS only caller (each platform's own `challenge()`) is
// still Windows-only until LU1c's `challenge_unix.rs` lands -- so, same
// `cfg_attr` reasoning as `host_handshake` just above, a non-Windows
// build still has no caller reaching this module yet.
#[cfg_attr(not(windows), allow(dead_code))]
mod deadline;
pub mod envelope;
// ADR 0041 step 6, unit U0 round-1 (blocker 3): the public facade over
// fsutil::lock_supervisor -- fsutil itself is a private module, invisible
// from any OTHER crate, including a future sot-capsule binary target.
pub mod fence;
// ADR 0041 step 6, unit U0 round-1: a pipe lane's post-SID identity
// exchange (encode request, decode reply) -- the one thing every
// platform's own `challenge()` delegates per-lane, via
// `challenge::exchange_identity`. Portable, like `deadline`.
pub mod exchange;
// ADR 0041 step 6, unit U3: the FE attach-only client's PURE state
// machines (the six FE rulings from "Step 6 as specified") -- portable,
// like `pointer`/`exchange`/`rollout`: no OS call, so it is genuinely
// tested on every CI platform. The Windows-only runtime that wires these
// to a real `PipeClient` lives in `fe_client_win`, gated by its own
// `#![cfg(windows)]`.
pub mod fe_client;
#[cfg(windows)]
pub mod fe_client_win;
// ADR 0041 step 6, unit U2: the supervisor's own durable operation
// journal (`operation_id`/`.active`/`.terminal`, recovery-first
// reconciliation) — portable, like `pointer`/`rollout`, since it reuses
// `fsutil::publish_noreplace` rather than any OS-specific primitive.
pub mod journal;
// ADR 0041 step 6, unit U2: the parent-death lease a spawned capsule
// checks as its first act after acquiring the writer fence — a named,
// kernel-brokered mutex, Windows-only. `pub`, matching `challenge_win`/
// `probe_win`: `tests/supervisor_win.rs` needs to reach it.
pub mod lease;
// ADR 0041 step 6, unit U0: `drawer.voyage` publication + validation.
// Portable (no OS-specific code): reuses `fsutil::publish_noreplace`,
// which already has both platform arms.
pub mod pointer;
pub mod record;
pub mod recovery;
// ADR 0041 step 6, unit U1b: the reader-first rollout gate for a
// feature-bearing segment (ADR 0039 registry) -- portable (no OS
// dependency), like `pointer`/`exchange`.
pub mod rollout;
pub mod segment;
// ADR 0041 step 6, unit U0 (promoted from the frontend's own paths.rs):
// the per-machine state-dir resolution rule, owned here so every process
// that needs it (today: the frontend) shares one rule instead of
// drifting copies.
pub mod state_dir;
// ADR 0041 step 6, unit U2: the authority -- `sot-capsule supervise`,
// and `endrun`/`reset` as fence-acquiring in-process callers. `pub`,
// matching `challenge_win`/`lease`/`probe_win`: Windows-only, and
// `tests/supervisor_win.rs` needs to reach it.
pub mod supervisor;
// ADR 0042 slice L1a: the small PRODUCTION supervisor-lane client for a
// non-FE, non-test caller (the backend daemon's own capsule workspace
// runtime) -- `pub`, matching `supervisor`/`challenge_win`/`fe_client_win`:
// Windows-only, and `sot-backend` (a separate crate) needs to reach it.
#[cfg(windows)]
pub mod supervisor_client;
pub mod verify;
pub mod voyage;
pub mod wire;

mod fsutil;

pub use envelope::*;
pub use record::{RecordKind, TailClass, CODEC_JSON, MAGIC, PRELUDE_LEN, RECORD_MAX_BODY};
pub use segment::{SegmentIdentity, SegmentReader, SegmentState, SegmentWriter};

/// Errors are split by what the caller may do about them: `TornTail` is the
/// ONLY recoverable corruption (ADR 0039 tail rule); everything else under
/// `Corrupt` requires an operator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Provably incomplete final record of an unsealed file (tail rule cases
    /// a/b). Recovery may truncate exactly this.
    #[error("torn tail at offset {offset}: {what}")]
    TornTail { offset: u64, what: &'static str },
    /// Any other defect: loud, never auto-repaired.
    #[error("corrupt at offset {offset}: {what}")]
    Corrupt { offset: u64, what: String },
    #[error("schema: {0}")]
    Schema(String),
    #[error("state: {0}")]
    State(String),
    /// U1a (ADR 0041 Lifecycle "Discovery, and the two windows"):
    /// `VoyageStore::open_for_writing_with_lease`'s caller-supplied
    /// parent-death lease reported itself already broken, checked as the
    /// FIRST act after the writer fence is acquired and before any history
    /// traversal. The fence has already been released by the time this
    /// error returns — the caller (a spawned child whose supervisor is
    /// already gone) must exit without binding, never retry or repair.
    #[error("parent-death lease broken; exiting without binding the voyage")]
    LeaseBroken,
    /// The voyage store requires OS durability primitives this platform
    /// lacks (Linux and Windows have real arms; others fail closed).
    #[error("unsupported on this platform: {0}")]
    Unsupported(&'static str),
    /// A ConPTY/job OPERATION failed (Windows-only: `conpty` module) — a
    /// spawn stage, or a later runtime call (`terminate`, `active_processes`,
    /// `resize`, ...). Carries WHICH operation and the underlying Win32
    /// error, so a caller can commit `producer_dead {spawn_failed}` for a
    /// creation-time failure, or a resize/teardown outcome for a later one,
    /// with a real diagnostic instead of a bare string. This variant is
    /// deliberately not named/worded "spawn" — an earlier version was, and a
    /// `resize()` failure read as a spawn failure it never was (review
    /// finding on the conpty unit).
    #[cfg(windows)]
    #[error("conpty: {0}")]
    Conpty(#[from] conpty::ConptyError),
    /// A named-pipe transport OPERATION failed (Windows-only: `pipe_win`
    /// module) — binding the pipe (`PipeServer::bind`, inside
    /// `PipeTransport::bind`) is the ONLY place `pipe_transport.rs`
    /// converts one of these into this crate's own `Error`. A LATER,
    /// background failure on an already-bound pipe (`pipe_win`'s own
    /// `TransportEvent::AcceptError`, its accept loop's persistent-failure
    /// signal) never reaches this type at all — `pipe_transport.rs`
    /// translates it to `transport::TransportEvent::TransportFatal`
    /// instead, delivered through `Transport::try_recv_event` like any
    /// other transport event, since by then `run` is already past `bind`
    /// and mid-loop, not somewhere a `Result` could propagate to.
    #[cfg(windows)]
    #[error("pipe transport: {0}")]
    Pipe(#[from] pipe_win::PipeError),
    /// A Unix-domain-socket transport OPERATION failed (Unix-only:
    /// `socket_unix` module) — binding the socket (`SocketServer::bind`,
    /// inside `SocketTransport::bind`) is the ONLY place
    /// `socket_transport.rs` converts one of these into this crate's own
    /// `Error` — mirrors the `Pipe` variant's own rule exactly. A LATER,
    /// background failure on an already-bound socket (`socket_unix`'s own
    /// `TransportEvent::AcceptError`) never reaches this type at all —
    /// `socket_transport.rs` translates it to
    /// `transport::TransportEvent::TransportFatal` instead, delivered
    /// through `Transport::try_recv_event` like any other transport
    /// event, since by then the capsule's run loop is already past `bind`
    /// and mid-loop, not somewhere a `Result` could propagate to.
    #[cfg(unix)]
    #[error("socket transport: {0}")]
    Socket(#[from] socket_unix::SocketError),
}

pub type Result<T> = std::result::Result<T, Error>;
