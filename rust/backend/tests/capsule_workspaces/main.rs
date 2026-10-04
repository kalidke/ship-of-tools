#![cfg(any(windows, target_os = "linux"))]
//! ADR 0042 slice L1a / ADR 0043 decision 22 (LU4) real-process
//! integration test: a real `sotd`, a real `sot-capsule[.exe]` it spawns
//! DETACHED, talking the actual wire protocol over a real local socket
//! (a named pipe on Windows, an `AF_UNIX` socket on Linux) — the same
//! posture `rust/log/tests/supervisor_win.rs`/`supervisor.rs` take for
//! the supervisor authority one layer down. Requires `sot-capsule[.exe]`
//! already built into the SAME target directory as `sotd[.exe]` (the CI
//! job builds the whole workspace first — see `.github/workflows/rust.yml`'s
//! `conpty-windows-2022` and `ubuntu-latest` jobs; production locates it
//! the identical way, next to the daemon's own executable). Every
//! `workspace.create` in this file requests `"runtime": "capsule"`
//! explicitly for clarity — since ADR 0042 L6 (this repo's B6 lane) it
//! is also this host's own default on Linux, same as Windows, but every
//! fixture here stays explicit so it reads on its own (see `ops/workspace.rs`'s
//! own doc on the field).
//!
//! Every wait below is a BOUNDED poll or `tokio::time::timeout` for an
//! external, observable fact (the socket accepting a connection, a
//! `workspace.list` row's own `phase` field, a supervisor lane going
//! silent) — never a sleep-and-hope, and never an unbounded read/write/
//! kill/wait (Codex review finding 13).
//!
//! Uses `sot_log::supervisor_client` directly (a real dependency of this
//! crate, not a test double) for two proofs the daemon's own wire
//! protocol has no op for: (1) `stop` ends JUST the supervisor authority
//! while its capsule leg survives (ADR 0041 Lifecycle — legs are
//! deliberately outside the supervisor's own job), which is how this
//! test proves ADOPTION (a fresh `--resume` finding the SAME leg still
//! alive, never bumping its epoch) rather than mere detachment (an
//! untouched, already-running supervisor merely surviving a daemon
//! restart); (2) `query_status` after `workspace.destroy` independently
//! confirms the record actually closed before this test ever asserts the
//! row is gone from `workspace.list`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sot_protocol::op;
// Linux-only: every `Command`/`Stdio` call site left in this file after
// the ADR 0045 lane B4b support-module lift (`kill_supervisor_only`,
// `kill_leg_only`, `count_matching_processes`) and every `Frame`/`codec::`
// call site (the `lane.connect` fixtures) live inside
// `#[cfg(target_os = "linux")]` helpers -- an unguarded import here would
// warn unused on Windows.
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use sot_protocol::{codec, Frame};
// LU5a: only the Linux-only unqualified-state-root refusal test below
// needs this -- Windows has no tmpfs-as-state-root concern (its own
// NTFS-only preflight is unrelated and unchanged), so an unguarded import
// here would warn unused on that leg.
#[cfg(target_os = "linux")]
use sot_protocol::slug;

// ADR 0045 lane B4b: `Env`, the wire-protocol round-trip helpers
// (`connect_and_hello`, `call`, `poll_until`, `poll_for_phase`), `find_row`,
// `try_query_status`, `create_ready_workspace_then_stop_its_supervisor`, and
// the anchored-pgrep leg-sweep machinery (`build_leg_pgrep_pattern` and
// friends, `Env::leg_pgrep_pattern`) moved verbatim to `tests/support/mod.rs`
// so `lane_bridge.rs`'s own cross-process proofs can reuse them without a
// second, drifting copy. `mod support;` (not a `tests/*.rs` file itself —
// Cargo only auto-discovers direct children of `tests/`) plus a glob import
// brings every lifted item back into this file's own scope, unchanged.
#[path = "../support/mod.rs"]
mod support;
use support::*;

/// Real-process tests share one CI runner; serialize them like
/// `supervisor_win.rs`'s own `SERIAL` — a spawned `sotd` plus a spawned
/// `sot-capsule` plus a spawned platform-shell leg is real load on a
/// two-core box. `tokio::sync::Mutex`, not `std::sync::Mutex`: this test
/// is async and holds the guard across `.await` points for its whole
/// body.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

mod attach;
mod create;
#[cfg(target_os = "linux")]
mod destroy;
#[cfg(target_os = "linux")]
mod ended_row;
#[cfg(target_os = "linux")]
mod lane_connect;
#[cfg(target_os = "linux")]
mod phase;
mod platform;
mod pty;
#[cfg(target_os = "linux")]
mod resume;
mod spawn;

#[cfg(target_os = "linux")]
#[cfg(test)]
mod leg_pgrep_pattern_tests {
    use super::*;

    /// G2's own regression case: a space in the executable path (a legal
    /// `CARGO_TARGET_DIR` with a space in it) must still produce a
    /// pattern that matches that path LITERALLY — a space is not an ERE
    /// metacharacter, so it must pass through `regex_escape_path`
    /// untouched rather than being dropped or mis-escaped.
    #[test]
    fn build_leg_pgrep_pattern_keeps_a_literal_space_in_the_exe_path() {
        let exe = Path::new("/scratch/build target/debug/sot-capsule");
        let state_root = Path::new("/tmp/sotcw-abc123/state");
        let pattern = build_leg_pgrep_pattern(&exe, "supervise", &state_root);
        assert_eq!(
            pattern,
            r"^/scratch/build target/debug/sot-capsule supervise /tmp/sotcw-abc123/state"
        );
    }

    /// Regex metacharacters in EITHER half (`+`/`.`) must be escaped so
    /// they match themselves literally rather than being interpreted by
    /// `pkill`/`pgrep`'s own POSIX ERE engine (a stray `.` would
    /// otherwise match any single character, widening the match rather
    /// than narrowing it to this exact path).
    #[test]
    fn build_leg_pgrep_pattern_escapes_regex_metacharacters_in_both_halves() {
        let exe = Path::new("/scratch/target+build/sot-capsule");
        let state_root = Path::new("/tmp/sotcw-v1.2/state");
        let pattern = build_leg_pgrep_pattern(&exe, "run", &state_root);
        assert_eq!(
            pattern,
            r"^/scratch/target\+build/sot-capsule run /tmp/sotcw-v1\.2/state"
        );
    }
}

// A DETACHED leg this test's own row may have left running (`stop` ends
// ONLY the supervisor AUTHORITY — ADR 0041 Lifecycle, legs are
// deliberately outside the supervisor's own job — so a test whose own
// teardown calls `stop` but never a matching `end_run` for the row's
// CURRENT voyage leaves its platform-shell leg orphaned on Linux) no
// longer needs a per-test sweep call: `Env`'s own `Drop` (F4, LU4 review
// round 2) sweeps every leg this env could have spawned, anchored on its
// own `state_root`, unconditionally, on every exit path — see that impl.

/// Whether the supervisor AUTHORITY (not its capsule leg) is stopped
/// before [`restart_daemon_and_prove_adoption`] kills and relaunches the
/// daemon — the one axis that distinguishes this file's two adoption
/// scenarios.
#[derive(Debug, Clone, Copy)]
enum AuthorityAtRestart {
    /// The field bug's exact precondition (daemon-boot-adopts-a-live-
    /// supervisor fix): the authority is left ALIVE, only the daemon
    /// process itself is killed. Proves the rebooted daemon ADOPTS the
    /// still-answering lane rather than racing a competing `--resume`
    /// into its fence.
    Alive,
    /// The authority is explicitly stopped first
    /// (`sot_log::supervisor_client::stop`) — its capsule leg
    /// deliberately survives (ADR 0041 Lifecycle: legs are outside the
    /// supervisor's own job). Proves `sot-capsule`'s OWN leg-adoption of
    /// a still-alive orphaned leg behind a genuinely DEAD lane.
    Stopped,
}

/// Shared "restart the daemon and prove adoption" body for both of this
/// file's adoption scenarios (round-2 Codex finding: one helper, not two
/// near-duplicate ~150-line test bodies). Takes the state a preamble
/// (`workspace.create` + poll-to-"ready", which cannot itself be shared
/// — each test owns an independent `Env`/daemon) has already produced,
/// `authority`-conditionally stops the supervisor authority, kills and
/// relaunches the daemon, and polls the new daemon's `workspace.list`
/// for `state_dir` to keep matching and phase to NEVER read "terminal"
/// — covers BOTH scenarios' own regression (a competing spawn racing a
/// still-held fence marks terminal; a genuinely dead lane's own resume
/// should never either) — until it reaches "ready", with EXTRA
/// post-ready dwell for [`AuthorityAtRestart::Alive`] (the field bug's
/// own timing: a competing spawn's watchdog saw its contention/terminal
/// exit within a couple hundred ms, so a plain "stop at the first ready"
/// poll could exit before a DELAYED terminal-mark regression ever showed
/// up — the `Stopped` scenario's resume is a real process spawn with no
/// fence contention at risk once it reports ready, so it gets no extra
/// dwell). Finally asserts the leg epoch is UNCHANGED across the restart
/// — the proof that whichever mechanism resumed the run ADOPTED it
/// rather than spawning a fresh contender. Returns the new connection/
/// next-id so a caller (today: only the `Stopped` scenario) can continue
/// past this point on the SAME connection — the new daemon itself needs
/// no return: `env` already owns it (`Env::spawn_sotd`, F4), so a later
/// `env.kill_daemon_bounded()` at the caller's own teardown reaps it.
async fn restart_daemon_and_prove_adoption(
    env: &Env,
    conn: Conn,
    workspace_id: &str,
    state_dir: &str,
    state_dir_path: &Path,
    leg_before: u64,
    authority: AuthorityAtRestart,
) -> (Conn, u64) {
    if matches!(authority, AuthorityAtRestart::Stopped) {
        tokio::task::spawn_blocking({
            let dir = state_dir_path.to_path_buf();
            move || sot_log::supervisor_client::stop(&dir).expect("stop the supervisor authority")
        })
        .await
        .unwrap();

        poll_until(
            || {
                let dir = state_dir_path.to_path_buf();
                async move {
                    if try_query_status(dir).await.is_none() {
                        Some(())
                    } else {
                        None
                    }
                }
            },
            BOUND,
            "the stopped supervisor's own lane to go silent",
        )
        .await;
    }

    env.kill_daemon_bounded().await;
    drop(conn);

    env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;

    let post_ready_dwell = match authority {
        AuthorityAtRestart::Alive => Some(Duration::from_secs(5)),
        AuthorityAtRestart::Stopped => None,
    };
    let ready_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let mut dwell_until: Option<Instant> = None;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        if let Some(row) = find_row(&payload, workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_eq!(
                row["state_dir"].as_str(),
                Some(state_dir),
                "the resumed/adopted row's state_dir must be the SAME capsule (authority={authority:?})"
            );
            let phase = row["phase"].as_str();
            assert_ne!(
                phase,
                Some("terminal"),
                "row went terminal across the daemon restart (authority={authority:?}) -- a competing \
                 leg was spawned into a still-held fence and lost"
            );
            if phase == Some("ready") && dwell_until.is_none() {
                match post_ready_dwell {
                    Some(d) => dwell_until = Some(Instant::now() + d),
                    None => break,
                }
            }
        }
        if let Some(dl) = dwell_until {
            if Instant::now() >= dl {
                break;
            }
        } else {
            assert!(
                Instant::now() < ready_deadline,
                "timed out waiting for the resumed/adopted row to reach phase \"ready\" (authority={authority:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.to_path_buf();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status after restart")
                .0
                .leg
        }
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed across the daemon restart (authority={authority:?}) -- a fresh/competing leg was spawned, not adopted"
    );

    (conn2, next_id2)
}

/// One `workspace.list` round trip's `state_dir` for `workspace_id` —
/// factored out of the big test above so its own long body reads as one
/// story rather than three inlined polls of the same shape.
async fn state_dir_from_list(conn: &mut Conn, next_id: &mut u64, workspace_id: &str) -> PathBuf {
    let id = *next_id;
    *next_id += 1;
    let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&payload, workspace_id).expect("workspace.list row for this workspace_id");
    PathBuf::from(row["state_dir"].as_str().expect("state_dir"))
}

// --- ADR 0043 decision 33 (lane L1a): the per-row guard, resume_if_absent,
// and the watchdog's guard-through-backoff restart --- //

/// SIGKILL every process matching the SUPERVISE half of this env's own
/// anchored leg pattern ([`build_leg_pgrep_pattern`]), then poll it gone —
/// simulates the authority crashing outright (never a graceful `stop`,
/// which would publish its own end-of-authority state cleanly). The
/// capsule LEG (a separate process, ADR 0041 Lifecycle) is untouched.
#[cfg(target_os = "linux")]
fn kill_supervisor_only(state_root: &Path) {
    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", state_root);
    let _ = Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a supervisor process still matches {pattern:?} after SIGKILL"
    );
}

/// [`kill_supervisor_only`]'s twin for the capsule LEG (the `run`
/// subcommand) — used only where a test needs the leg genuinely gone too
/// (no marker, no survivor to adopt), never on its own.
#[cfg(target_os = "linux")]
fn kill_leg_only(state_root: &Path) {
    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", state_root);
    let _ = Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a leg process still matches {pattern:?} after SIGKILL"
    );
}

/// The number of live processes whose command line matches `pattern` —
/// [`any_process_matches`]'s counting twin, needed by the stale-attach
/// test below to prove "at most ONE," not merely "at least one." `Err`
/// only when `pgrep` itself could not be run at all (Codex review,
/// 2026-09-11: a query failure must fail the test, never silently count
/// as "zero processes" — a false "at most one" proves nothing). `pgrep`
/// exiting 1 (no match) is a normal, successful `Ok(0)`, not an error.
#[cfg(target_os = "linux")]
fn count_matching_processes(pattern: &str) -> std::io::Result<usize> {
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.trim().is_empty()).count())
}

/// How many times `needle` appears in `sotd.log` so far — used to
/// synchronize on a NEW occurrence of a specific watchdog log line
/// (Codex review, 2026-09-11) rather than a fixed sleep, which proves
/// nothing about whether the watchdog has actually reached the point in
/// its own code that line marks.
#[cfg(target_os = "linux")]
fn count_log_occurrences(log_path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log_path).map(|s| s.matches(needle).count()).unwrap_or(0)
}

/// Count of marker files under `dir` (R4f arrival/completion count). Missing
/// dir reads as zero, not an error.
#[cfg(target_os = "linux")]
fn count_dir_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|entries| entries.filter_map(|e| e.ok()).count()).unwrap_or(0)
}
