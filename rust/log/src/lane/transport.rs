//! The lanes' transport pieces: the connect bound and the error type every client reports.
//!
//! Deliberately ungated: this is the contract, not an implementation.

use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The aggregate budget the capsule's closer and reader joins share: expiry is loud, and a thread still unfinished
/// stays unjoined.
pub const TEARDOWN_AGGREGATE_DEADLINE: Duration = Duration::from_secs(20);

/// [`join_within`]'s poll granularity: small enough that a fast, healthy teardown never visibly waits for it.
pub(crate) const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Wait for `jh` to finish without ever calling the BLOCKING `JoinHandle::join`, which std gives no timeout. Polls
/// [`JoinHandle::is_finished`] until it reports true (join it for real, which cannot then block, and return `true`) or
/// `deadline` passes (`false`: LOUD, since this crate cannot force an OS thread to stop; the handle is dropped, which
/// detaches rather than kills it). `is_finished` is checked FIRST on every poll, including the one that observes the
/// expired deadline, so a thread that finished at or before that instant is accepted even though the deadline has
/// also passed: the invariant is "never proceed past an UNFINISHED worker", and a finished thread leaves nothing
/// dangling. A thread that panicked still counts as finished.
pub(crate) fn join_within(jh: JoinHandle<()>, deadline: Instant) -> bool {
    loop {
        if jh.is_finished() {
            let _ = jh.join();
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(JOIN_POLL_INTERVAL);
    }
}

/// The total connect retry budget, shared by both platforms' clients (ADR 0043 decision 27): an endpoint that is
/// BUSY (a pipe instance already claimed, `ERROR_PIPE_BUSY`, or a Unix listener whose backlog is momentarily full or
/// whose accept was merely interrupted, `EAGAIN`/`EINTR`) is retried within it, because a listener holds its instances
/// continuously and recycles them once bound, so "unavailable" past that point can only mean busy. An ABSENT endpoint
/// (no pipe instance exists yet, `ERROR_FILE_NOT_FOUND`, or no listener at all, `ECONNREFUSED`/`ENOENT`) returns on the
/// very first attempt: this bound is never charged waiting for a server that has not started yet. Whoever knows a
/// server is coming (the daemon's post-spawn poll, a viewer's own episode loop, a test harness) polls for that at its
/// own interval instead.
pub const CONNECT_BOUND: Duration = Duration::from_secs(2);

/// Errors a client's synchronous API surface can report at the call site: the union of both platforms' variants, with
/// `Io { op, source }` kept as the shape production code matches on, and the Unix-only `PathTooLong`/`RuntimeDir`
/// carried unconditionally (an enum variant costs nothing on a platform that never produces it).
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    // Unix only (never produced on Windows, where there is no `sun_path` limit to exceed). The exact byte ceiling is
    // platform-specific, so the message names the path without embedding a number this type cannot portably compute.
    #[error("endpoint path {0:?} exceeds this platform's path-length limit")]
    PathTooLong(PathBuf),
    #[error("resolving the runtime dir: {0}")]
    RuntimeDir(std::io::Error),
    #[error("{op}: {source}")]
    Io {
        op: &'static str,
        source: std::io::Error,
    },
    #[error("empty payload: a write never carries a zero-length send")]
    EmptyPayload,
    #[error("payload of {0} bytes exceeds what a single write/read call can represent")]
    PayloadTooLarge(usize),
    #[error("operation cancelled")]
    Cancelled,
    /// A second same-direction client call (e.g. two concurrent `read`s)
    /// was rejected before it ever touched the OS or a shared I/O slot —
    /// misuse, not a race this crate resolves for the caller.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
    /// A connect-time peer-identity check (ADR 0041 Lifecycle "The
    /// challenge", steps 1-3 — Windows: token-user SID comparison via
    /// `challenge_win::authenticate_server`; Linux: `SO_PEERCRED` same-
    /// user comparison via `challenge_unix::authenticate_server` — NOT
    /// the full five-step `challenge()`, see either function's own doc)
    /// answered with a WELL-FORMED WRONG proof — a different account's
    /// process is behind this endpoint. A loud, typed failure: never
    /// retried as if the peer might still turn out legitimate.
    #[error("the peer failed identity authentication (a different account's process is behind this endpoint)")]
    Foreign,
    /// Peer identity authentication could not be completed — `via`
    /// names WHICH steps-1-3 check came back inconclusive: `"direct"`,
    /// an OS-call failure in a platform endpoint's own steps 1-3 on a
    /// direct connect (ADR 0041: "a failure... is PENDING, never READY
    /// and never ADOPTED"), or `"bridge"` (ADR 0045 decision 4), the
    /// DAEMON's own steps-1-3 on a lane-bridge dial answering
    /// `undetermined` — this endpoint never runs its own OS-level check
    /// at all; the daemon does that instead (decision 3). `detail` is
    /// the check's own message. ONE variant: the two are the same
    /// OUTCOME (identity undetermined, never treated as authenticated
    /// or foreign) at two different STEPS on two different connections
    /// — `via` is what tells a caller which, not a second variant name.
    /// (Named `via`, not `source`: `thiserror` treats a field literally
    /// named `source` as the std `Error::source()` chain, which a plain
    /// `&'static str` cannot satisfy.)
    #[error("peer identity undetermined ({via}): {detail}")]
    Undetermined { via: &'static str, detail: String },
    /// ADR 0045 decision 4: the daemon behind a lane-bridge dial
    /// (`sot-protocol`'s `DaemonLaneEndpoint`) answered `lane.connect`
    /// with a REFUSAL — `code` is the wire value verbatim
    /// (`unknown_workspace`, `not_capsule`, `bad_lane`, `unauthenticated`,
    /// `foreign`, or `no_bridge` for a reply that did not even carry a
    /// recognizable `lane.connect` result, e.g. an old daemon's
    /// unknown-op answer). Terminal: never retried as if the daemon
    /// might change its mind.
    #[error("the daemon refused the lane ({code}): {detail}")]
    Refused { code: String, detail: String },
    /// ADR 0045 decision 4: the lane-bridge dial or handshake itself
    /// failed — connect timed out/refused, or the daemon never answered
    /// within the handshake bound. Distinct from [`Self::Io`] (which
    /// [`Self::is_endpoint_absent`] can read as a genuine absence):
    /// transport uncertainty must never be charged to that absence
    /// window, so callers match this BEFORE any conversion that would
    /// unwrap it back into a bare `io::Error`.
    #[error("the daemon's lane bridge is unreachable: {0}")]
    Unreachable(std::io::Error),
    /// ADR 0045 decision 4: the host's link is down — its control
    /// transport's last attempt got no hello reply — so no ssh was
    /// started. The attach worker's supervisor dial in `run_worker` and its
    /// voyage dials in `converge_on_ready` pause on it rather than retry on their own
    /// clock; every other connect site treats it as a failed dial under its
    /// existing pacing.
    #[error("the host's link is down; no ssh was started")]
    LinkDown,
}

impl TransportError {
    /// L1-unix LU3c (ADR 0043 decision 21): true iff this error means
    /// "the endpoint is not there right now" — a pipe name that does not
    /// exist (Windows: `NotFound`) or a Unix domain socket path with no
    /// listener (`ConnectionRefused` — the kernel refuses a connect
    /// against a bound but unlistened, or unlinked, path; a path that was
    /// never created at all is `NotFound` there too). The supervisor's
    /// own `end_run_over_mgmt_lane`/`probe_writer_liveness` use this ONE
    /// predicate on both platforms instead of the former inline
    /// `NotFound`-only guard: Windows behavior is unchanged (a named
    /// pipe connect never yields `ConnectionRefused`), so this widens
    /// nothing there.
    pub fn is_endpoint_absent(&self) -> bool {
        matches!(
            self,
            TransportError::Io { source, .. }
                if matches!(source.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
        )
    }
}
