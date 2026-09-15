// server.rs — listener orchestration + transport-agnostic per-connection task.
//
// Spawns the local-socket listener (interprocess) per the Opts the user
// passed. Each accepted stream gets split into AsyncRead/AsyncWrite halves
// and handed to a generic `handle_connection`. The daemon TCP listener (and
// its app-token gate) was removed in 0.4.0 — see ADR 0010's update block;
// the socket's boundary is OS ownership of its private parent path, and
// remote access is an SSH local-forward terminating at the socket.
//
// Conventional socket strings:
//   Linux/Mac: filesystem path,  e.g. `/tmp/sot-spike.sock`
//   Windows:   named pipe,       e.g. `\\.\pipe\sot-spike`
// interprocess accepts both verbatim via `GenericFilePath::to_fs_name`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath, ListenerOptions,
};
use sot_protocol::{
    codec, op, FeCommandEvt, Frame, HostLatest, Kind, MonitorHistoryReq, MonitorHistoryRes,
    MonitorSubscribeRes, MonitorTickEvt, PtyOpenReq,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::clients::{ClientGuard, Clients};
use crate::concept::ConceptStore;
use crate::files_mode::FilesMode;
use crate::handlers;
use crate::kernel::Kernel;
use crate::mathjax::MathJax;
use crate::paths;
use crate::pluto::Pluto;
use crate::repl::{Repl, ReplFrameMsg};
use crate::session::Session;
use crate::watcher::PreviewChanged;
use crate::workspaces::AgentMessage;
use crate::workspaces::WorkspaceChanged;
use crate::workspaces::{self, Workspace, Workspaces};
use crate::Opts;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinSet;

// Half-open connection reaper tunables (ADR 0027). A peer that dies without a
// FIN — a frontend killed -9, a collapsed SSH local-forward, a yanked network
// — leaves the daemon-side socket ESTAB forever; without these two mechanisms
// its task leaks (fd + ClientGuard) and, if blocked mid-write, never reads the
// socket again (the broadcast-stall we hit). See `write_frame_to`
// (write-timeout). The keepalive half lived in the TCP listener and retired
// with it in 0.4.0 — on the local socket, a dead SSH forward closes the
// stream (EOF) rather than leaving it silently half-open.

/// Base deadline for a single frame write before we treat the peer as dead and
/// drop the connection. Small control frames — even over the SSH tunnel — drain
/// in well under a second, so 10s catches a wedged/non-draining peer fast without
/// parking a connection task. A rare false drop is cheap: the FE reconnects
/// automatically (exponential backoff).
///
/// This is a FLOOR, not the whole story: a legitimate bulk blob (a 71 MB
/// scientific render riding the codec's blob tail) can't drain in 10s over a
/// tunnel, and a flat 10s per-frame cap false-dropped it mid-write — the reaper
/// firing on a transfer that WAS draining, so the preview silently never arrived
/// (2026-06-30, example-paper render_sr.png). The deadline is therefore SCALED by
/// the blob size (see `write_deadline`): base + size/`MIN_BLOB_DRAIN_RATE`.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Floor drain rate we credit a healthy peer for the bulk blob tail. A real
/// transfer sustains far more than this over a tunnel; setting the floor low
/// keeps the size-scaled deadline generous enough never to false-drop a draining
/// blob, while a genuinely wedged peer (zero progress) is still bounded here.
/// 1 MiB/s → a 71 MB render gets ~71s on top of the base floor.
const MIN_BLOB_DRAIN_RATE: u64 = 1024 * 1024; // bytes/sec

/// Per-frame write deadline: the [`WRITE_TIMEOUT`] floor plus one second of grace
/// per `MIN_BLOB_DRAIN_RATE` bytes of blob. Envelope-only / small-blob frames get
/// the tight floor (reaper stays sharp); a large preview blob gets proportional
/// time to drain so a legit transfer isn't reaped mid-write.
fn write_deadline(blob: Option<&[u8]>) -> std::time::Duration {
    let extra = blob.map_or(0, |b| b.len() as u64 / MIN_BLOB_DRAIN_RATE);
    WRITE_TIMEOUT + std::time::Duration::from_secs(extra)
}

/// Read deadline for a connection whose declared role is `fe` or `bridge`
/// (topology plan §F step 2, D10 — the half-open-roster fix). Since 0.4.0
/// the daemon has had no keepalive, and `WRITE_TIMEOUT` above never fires
/// for a tunnelled peer (its writes always drain into sshd's Unix-socket
/// side) — a closed laptop stayed in the roster for 15 min to 2 h. No
/// frame AT ALL from the peer within this long means treat the connection
/// as dead: drop it and reap it through the same `ClientGuard::drop` path
/// as a clean exit (`clients.rs:329`). Three times the client-side `ping`
/// interval (30s — the frontend transport and `comm-listen.sh`'s bridge
/// loop): one missed tick is noise, three in a row is a dead peer.
/// `cli`/`agent` (one-shot) connections never gate on this — see
/// `is_long_lived_role` at its declaration site.
const PING_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// `SOT_TEST_PING_READ_DEADLINE_MS` overrides [`PING_READ_DEADLINE`] for
/// tests — same `OnceLock`-cached-once-per-process convention as
/// `test_slow_concept_read_delay` (read once, before any connection can
/// have started, never something a client controls per-request). Unset in
/// every real deployment.
fn ping_read_deadline() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_PING_READ_DEADLINE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(PING_READ_DEADLINE)
}

/// Write one frame to a connection with a bounded timeout (ADR 0027, reaper
/// half 2). On timeout we return an error so `handle_connection` unwinds and
/// drops the connection: a peer that hasn't drained a single frame in
/// `WRITE_TIMEOUT` is dead or wedged, and a parked write would otherwise hold
/// the task forever — never reading the socket, never releasing its
/// `ClientGuard`. Cancel-safety is irrelevant on the timeout path: we tear the
/// whole socket down, so a partially written frame is moot. Every
/// per-connection evt/response write goes through this.
async fn write_frame_to<W>(tx: &mut W, frame: &Frame, blob: Option<&[u8]>) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_frame_within(tx, frame, blob, write_deadline(blob)).await
}

/// Timeout-parameterized core of [`write_frame_to`], split out so the reaper's
/// drop-on-stuck-peer behavior is unit-testable in milliseconds rather than the
/// production `WRITE_TIMEOUT`.
async fn write_frame_within<W>(
    tx: &mut W,
    frame: &Frame,
    blob: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, codec::write_frame(tx, frame, blob)).await {
        Ok(inner) => {
            inner?;
            Ok(())
        }
        Err(_elapsed) => anyhow::bail!(
            "frame write exceeded {timeout:?}; dropping connection (peer not draining)"
        ),
    }
}

/// Write one outgoing frame — an inline reply or an off-loop job's reply
/// alike — with the SAME containment `handle_connection`'s dispatch loop has
/// always applied: an over-cap envelope degrades to an error frame for that
/// request instead of ending the connection (`codec::write_frame` validates
/// size before writing a single byte, so nothing reached the wire and the
/// stream is still consistent); every other write failure, most notably the
/// write-timeout "peer not draining" bail, still propagates so the caller's
/// `?` ends the connection exactly as it always did.
async fn write_reply<W>(tx: &mut W, frame: Frame, blob: Option<Vec<u8>>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if let Err(e) = write_frame_to(tx, &frame, blob.as_deref()).await {
        if let Some(too_large) = e.downcast_ref::<codec::EnvelopeTooLarge>() {
            tracing::warn!(
                op = %frame.op,
                id = frame.id,
                len = too_large.len,
                cap = too_large.cap,
                "response envelope over cap — answering with error frame, keeping connection"
            );
            let payload = serde_json::json!({
                "error": format!("{too_large}"),
                "code": "envelope_too_large",
            });
            let err_frame = Frame::res(frame.id, &frame.op, payload);
            write_frame_to(tx, &err_frame, None).await?;
            return Ok(());
        }
        return Err(e);
    }
    Ok(())
}

/// Per-request service-time logging (`SLOW_REQUEST_MS`) plus the error
/// containment that turns a handler `Err` into one `handler_error` frame for
/// `req_id` — shared by the inline dispatch path and every off-loop job
/// (`spawn_job`) so the two don't carry separate copies of the same
/// bookkeeping.
fn finish_dispatch(
    op_name: &str,
    req_id: u64,
    transport: &'static str,
    started: std::time::Instant,
    result: Result<handlers::HandlerOutput>,
) -> handlers::HandlerOutput {
    let service_ms = started.elapsed().as_millis() as u64;
    if service_ms >= SLOW_REQUEST_MS {
        tracing::info!(op = %op_name, id = req_id, service_ms, transport, "slow request");
    } else {
        tracing::debug!(op = %op_name, id = req_id, service_ms, "request served");
    }
    match result {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!(
                op = %op_name,
                id = req_id,
                error = format!("{e:#}"),
                "handler error — answering with error frame, keeping connection"
            );
            let payload = serde_json::json!({
                "error": format!("{e:#}"),
                "code": "handler_error",
            });
            vec![(Frame::res(req_id, op_name, payload), None)]
        }
    }
}

/// One outgoing job reply: a frame and its optional trailing blob. Off-loop
/// jobs (`spawn_job`) have no access to `tx` — it stays owned by
/// `handle_connection`'s own loop, the connection's one writer — so a job
/// hands its finished reply back over this channel instead; the loop drains
/// it and calls `write_reply` itself, same as it does for its own inline
/// replies.
type OutTx = mpsc::Sender<(Frame, Option<Vec<u8>>)>;

/// Per-connection cap on concurrently RUNNING off-loop jobs (`preview.get`,
/// `concept.read`, `image.crop`, `kernel.request`). Names the invariant it
/// protects: one connection's burst of these can't starve the tokio
/// runtime's worker threads for every OTHER connection. Acquired INSIDE
/// each spawned job, never before spawning, so request intake itself is
/// never blocked by the cap — only how many jobs run at once, once already
/// queued. `pty.*` ops never touch this semaphore at all — they dispatch
/// INLINE (see the `op::PTY_*` arms below), which is what keeps them served
/// even while every slot here is busy (see `switch_latency.rs`'s
/// `pty_not_starved::pty_screen_is_served_while_a_real_slow_kernel_request_is_pending`
/// test).
const OFFLOOP_CONCURRENCY: usize = 4;

/// Cap on how long a queued off-loop job may wait for its `job_sem` PERMIT
/// before being discarded outright — never running the handler at all.
/// Without this, N jobs queued behind a live-but-hung kernel each wait
/// successive `OFFLOOP_CONCURRENCY`-sized batches with no overall bound: 40
/// requests could occupy ~10 batches in a row before the last one even
/// starts. Matches `KERNEL_REQUEST_TIMEOUT`'s own bound, so a request that
/// would time out anyway during its internal wait doesn't also waste a
/// queue slot first waiting to even begin.
const OFFLOOP_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn one request's handler as its own task, off this connection's
/// read/dispatch loop (switch-latency Phase 1): a slow `preview.get` no
/// longer delays a later cheap request's reply on the same connection. `fut`
/// is the actual handler call, already bound to its own owned copies of
/// whatever it needs (this task outlives the loop iteration that spawned
/// it). A panic inside `fut` unwinds this whole task — caught by the
/// `JoinSet` as a `JoinError`, which `handle_connection`'s own
/// `jobs.join_next()` arm logs; that mirrors the inline dispatch path, which
/// has never had panic containment either. Delivers its result through
/// `out_tx` for the loop to write, exactly like an inline reply.
///
/// The deadline (`OFFLOOP_QUEUE_TIMEOUT`) starts HERE, before the permit is
/// even requested — not after acquiring it — so a job queued behind a
/// saturated cap for too long is discarded with the standard timeout error
/// and `fut` never runs, rather than finally starting once nobody still
/// cares about the answer.
fn spawn_job<F>(
    jobs: &mut JoinSet<()>,
    semaphore: Arc<Semaphore>,
    out_tx: OutTx,
    req_id: u64,
    op_name: String,
    transport: &'static str,
    fut: F,
) where
    F: std::future::Future<Output = Result<handlers::HandlerOutput>> + Send + 'static,
{
    let started = std::time::Instant::now();
    jobs.spawn(async move {
        let permit = match tokio::time::timeout(OFFLOOP_QUEUE_TIMEOUT, semaphore.acquire_owned())
            .await
        {
            Ok(p) => p.expect("connection job semaphore is never closed"),
            Err(_) => {
                let err = anyhow::anyhow!(
                    "{op_name} timed out after {OFFLOOP_QUEUE_TIMEOUT:?} waiting to run \
                     (off-loop queue saturated)"
                );
                let out_frames = finish_dispatch(&op_name, req_id, transport, started, Err(err));
                for (frame, blob) in out_frames {
                    let _ = out_tx.send((frame, blob)).await;
                }
                return;
            }
        };
        let out_frames = finish_dispatch(&op_name, req_id, transport, started, fut.await);
        drop(permit);
        for (frame, blob) in out_frames {
            if out_tx.send((frame, blob)).await.is_err() {
                // The connection loop is gone — nothing left to deliver.
                break;
            }
        }
    });
}

/// Resolve `payload`'s `workspace_id` hint to a concrete workspace INLINE,
/// before a request is handed to an off-loop job, and rewrite the hint to
/// that workspace's canonical id. Without this, a job queued behind others
/// (semaphore contention) resolves its workspace only once it actually runs
/// — if the hinted slug's workspace was destroyed and a new one created
/// reusing the SAME slug in the meantime, the job would silently bind to the
/// replacement. Canonical ids are never reused, so re-resolving by id at
/// execution time (the handler's own first step) either finds the SAME
/// workspace or correctly reports it gone — never someone else's.
///
/// Returns `Ok(true)` with `payload` rewritten when resolution succeeds,
/// `Ok(false)` after already answering the same `unknown_workspace` error
/// frame the handler itself would have sent — the caller just `continue`s
/// without spawning anything. `Err` only on a write failure serious enough
/// to end the connection.
async fn canonicalize_workspace_id<W>(
    tx: &mut W,
    workspaces: &Workspaces,
    req_id: u64,
    op_name: &str,
    payload: &mut serde_json::Value,
) -> Result<bool>
where
    W: AsyncWrite + Unpin,
{
    let hint = payload
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match workspaces.resolve(hint.as_deref()) {
        Some(ws) => {
            if let serde_json::Value::Object(m) = payload {
                m.insert(
                    "workspace_id".to_string(),
                    serde_json::Value::String(ws.workspace_id.clone()),
                );
            }
            Ok(true)
        }
        None => {
            let err_payload = serde_json::json!({
                "error": format!("unknown workspace: {hint:?}"),
                "code": "unknown_workspace",
            });
            write_reply(tx, Frame::res(req_id, op_name, err_payload), None).await?;
            Ok(false)
        }
    }
}

/// Test-only knob (switch-latency Phase 1): an integration test needs ONE
/// request it can make deterministically slow, through the real wire
/// protocol, to prove a later cheap request's reply doesn't wait behind it
/// on the same connection — no existing op is slow on demand without
/// something a CI sandbox can't assume (a real Julia kernel, a large file
/// already on disk). Reads `SOT_TEST_SLOW_CONCEPT_READ_MS` ONCE per process
/// via `OnceLock`, so it can only ever be a fixed value a test harness set
/// before spawning the daemon — never something a client controls
/// per-request. Unset (or unparseable) is `0`, a no-op sleep: production
/// never sets this, so every real deployment gets zero delay.
fn test_slow_concept_read_delay() -> std::time::Duration {
    static DELAY_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *DELAY_MS.get_or_init(|| {
        std::env::var("SOT_TEST_SLOW_CONCEPT_READ_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    });
    std::time::Duration::from_millis(ms)
}

/// Writes one marker file per arrival/completion/wait-for-settle-cycle
/// under `<barrier path>.<kind>/`, so a test can poll an exact count
/// instead of inferring one from timing. `pub(crate)` -- also called
/// from `capsule_workspace::ensure_started`'s own reprobe loop (kind
/// `"waitforsettle"`), which is a different module but shares this
/// exact barrier-path convention. No-op unless `SOT_TEST_ACTIVATION_
/// BARRIER` is set.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn record_test_activation_marker(kind: &str) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    let dir = std::path::PathBuf::from(format!("{barrier_path}.{kind}"));
    let _ = std::fs::create_dir_all(&dir);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("{}-{seq}", std::process::id())), b"");
}

/// Test-only barrier at the top of `pty.open`'s activation task: when
/// `SOT_TEST_ACTIVATION_BARRIER` names a path, blocks until the test
/// creates that file (not a guessed sleep), giving up past a 30s bound.
/// No-op in production.
#[cfg(any(windows, target_os = "linux"))]
async fn wait_for_test_activation_barrier() {
    let Ok(path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    record_test_activation_marker("arrivals");
    let path = std::path::PathBuf::from(path);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !path.is_file() {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(path = ?path, "capsule activation test barrier: released by timeout, not by the test");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}


/// Canonical projection of just the state-relevant fields per sot-comm
/// registry agent (state/summary/status_at, plus `host` — LU5d2:
/// `workspace.list`'s registry reads are now host-filtered, so a `host`
/// edit alone, e.g. a stale row's
/// owner changing, is a real change even when every other field is
/// unchanged). `last_seen` is deliberately excluded. Used by the
/// registry-watch task in `run` (below) to detect a real change between
/// polls; hoisted to module scope (out of that task's async block) so it's
/// unit-testable on its own. Deliberately NOT host-filtered (unlike the
/// list itself): this has no per-workspace context to filter against, only
/// a flat agent map, so a write on another host still costs one extra
/// `workspace.list` broadcast — cheaper than threading `declared_host()`
/// through a projection whose only job is "did anything change".
fn project_comm_registry(bytes: &[u8]) -> String {
    let root: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let agents = match root.get("agents").and_then(|a| a.as_object()) {
        Some(o) => o,
        None => return String::new(),
    };
    let mut keys: Vec<&String> = agents.keys().collect();
    keys.sort();
    let mut s = String::new();
    for k in keys {
        let e = &agents[k];
        s.push_str(k);
        for field in ["state", "summary", "status_at", "host"] {
            s.push('\u{1}');
            s.push_str(e.get(field).and_then(|v| v.as_str()).unwrap_or(""));
        }
        s.push('\u{2}');
    }
    s
}

pub async fn run(opts: Opts) -> Result<()> {
    // ADR 0046 decision 1: resolve this daemon's declared host at boot,
    // fatal if it can't be named, so the failure is a boot error rather
    // than a per-hello one. Pin the (bare, S4) own-listener endpoint from
    // `opts.socket` before it's consumed by value below; it is read by
    // `pty::awareness_env` for every pane/capsule this daemon ever spawns
    // (SOT_SOCKET only — the declared host is never pinned into a spawned
    // child's env; see `awareness_env`'s own doc).
    let _ = crate::workspaces::declared_host();
    if let Some(path) = opts.socket.as_deref() {
        crate::awareness::set_own_endpoint(path);
    }

    let session = Session::new();
    let (sid, _) = session.snapshot().await;
    tracing::info!(session_id = %sid, "session ready");

    let files_mode = Arc::new(FilesMode::new(opts.project_root.clone())?);
    tracing::info!(project_root = ?files_mode.root_path(), "files-mode ready");

    // Workspace registry (ADR 0014). Read every persisted workspace off
    // disk, then synthesize and register the *default* workspace (the
    // one this daemon was launched with, rooted at `--project-root`).
    // The default-id resolves to whichever workspace_id matches; for a
    // fresh first-launch we generate one and persist it so subsequent
    // runs see the same id.
    let workspaces = Workspaces::new();
    // A plain "no toml found" is already fail-soft inside `scan_disk`
    // (`Ok(0)`, never `Err`) — the only realistic source of an `Err` here
    // is the Windows legacy-config-dir migration's refuse-and-record path
    // (`workspaces::migrate_legacy_windows_config_dir`'s doc): a rename
    // that fails partway leaves the registry split across the old and new
    // roots, and continuing with whatever landed at the new root (empty or
    // partial) would silently seed a fresh registry beside a stranded one.
    // So this is a boot error, not a warning.
    let n = workspaces::scan_disk(&workspaces, opts.adopt_legacy_registry)
        .context("scanning the workspace registry")?;
    tracing::info!(count = n, "workspaces scanned from disk");
    let default_label = opts
        .label
        .clone()
        .or_else(|| {
            files_mode
                .root_path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "home".to_string());
    // ADR 0042 slice L1a, Codex review finding 5: the default workspace's
    // OWN `runtime` must survive this re-registration. `scan_disk` (just
    // above) already loaded it correctly from its toml if one exists —
    // read it back BEFORE constructing a fresh seed, whose own
    // `Workspace::from_label` default ("tmux") would otherwise silently
    // clobber a scanned capsule default back to tmux on every restart
    // (`insert`'s own "new metadata wins" semantics, working exactly as
    // designed, applied to the wrong source of truth). `None` means a
    // genuinely first-ever launch on this machine.
    //
    // Rule G (shrink round): the SAME clobber risk applies to the launch
    // fields — `insert`'s own doc ("the rest of the metadata is taken
    // from the new ws") means whatever `from_label` builds here REPLACES
    // the persisted row's `agent`/`agent_name`/`autostart_claude`/`task`
    // on EVERY restart, not just at create time. An existing default
    // row's persisted launch fields must survive re-registration the
    // same way its runtime does (below), computed here BEFORE
    // construction rather than patched after, since `from_label` takes
    // them as constructor args.
    let existing_default = workspaces.resolve(Some(&paths::slug(&default_label)));
    // ADR 0042 amendment (2026-09-04) governs a FIRST-EVER row only: the
    // preserve arm below keeps an existing default row's launch fields
    // verbatim, so a box whose row a pre-amendment daemon had already
    // stamped with an agent keeps behaving as before — a visible, startable
    // session at the home root — with no signal that a one-time cleanup is
    // owed (field day 2026-09-05: found by forensics on a Windows box). Say
    // so at boot, once, naming the remedy; never rewrite the row (it may be
    // a session the user is relying on).
    if let Some(existing) = &existing_default {
        if existing.runtime == "capsule" && existing.agent() != "none" {
            tracing::warn!(
                workspace_id = %existing.workspace_id,
                agent = %existing.agent(),
                toml = %workspaces::toml_path_for(&existing.slug).display(),
                "default workspace carries an agent, so it lists and starts as an ordinary session \
                 (a pre-2026-09-04 seed, or a deliberate choice); to make it the inert anchor: stop \
                 the daemon, set agent = \"none\" and autostart_claude = false in that toml, start again"
            );
        }
    }
    // 2026-09-04 amendment (owner ruling): the daemon's own home/default
    // row is an INERT ANCHOR — the workspace it falls back to and the
    // way to browse this machine's files, not a session — so a
    // genuinely first-ever launch seeds no agent and no autostart on
    // every host alike (before this amendment, Windows seeded
    // `agent = "claude"`, `autostart_claude = true` here, so pressing
    // Enter on it silently started a claude capsule and the row looked
    // like every other session — the exact confusion this amendment
    // removes). `default_row_launch_seed` (workspaces.rs, the launch-field
    // counterpart of `default_row_runtime` below) is the one place this
    // decision — and the Windows corrupted-row re-seed's OWN identical
    // fallback — is made, so it stays unit-testable without a live
    // registry.
    let existing_agent = existing_default.as_ref().map(|e| e.agent());
    let existing_agent_name = existing_default.as_ref().map(|e| e.agent_name());
    let (seed_autostart, seed_agent, seed_agent_name, seed_task) =
        workspaces::default_row_launch_seed(existing_default.as_deref().map(|e| {
            (
                e.autostart_claude,
                existing_agent.as_deref().unwrap_or_default(),
                existing_agent_name.as_deref().unwrap_or_default(),
                e.task.as_str(),
            )
        }));
    let mut default_ws_seed = Workspace::from_label(
        &default_label,
        files_mode.root_path().to_path_buf(),
        seed_autostart,
        seed_agent,
        seed_agent_name,
        seed_task,
    );
    // ADR 0042 slice L1a: route through the ONE function that decides
    // this row's runtime for this OS (`workspaces::default_row_runtime`
    // — see its own doc) rather than re-deciding it here. On Windows
    // this is unconditionally "capsule", correcting rather than
    // preserving a stale on-disk "tmux" leftover — the field incident
    // this fixes: the old preserve-verbatim behaviour never self-healed
    // such a value, and the daemon then refused to start the row at all
    // (`pty spawn failed error=tmux is not available on Windows`), a
    // dead end (`default_workspace_not_destroyable`, below).
    if let Some(existing) = &existing_default {
        if cfg!(windows) && existing.runtime != "capsule" {
            tracing::info!(
                workspace_id = %existing.workspace_id,
                on_disk_runtime = %existing.runtime,
                "default workspace runtime on Windows must be capsule (ADR 0042 L1a); \
                 correcting a stale on-disk value and re-seeding its agent/autostart \
                 to the inert anchor defaults (a corrupted row's launch fields, not \
                 just its runtime)"
            );
        }
    }
    // Manager review (S16, Codex finding S16): carry the existing row's
    // declared `agent_handle` (ADR 0046 decision 1's `agent.join`)
    // forward the same way `runtime` is above — `from_label` seeds a
    // fresh row with none at all, so without this every boot silently
    // wiped a default row's already-joined handle on the very next save.
    if let Some(existing) = &existing_default {
        default_ws_seed.agent_handle = std::sync::Mutex::new(existing.agent_handle());
    }
    workspaces.insert(default_ws_seed);
    let default_ws = workspaces
        .resolve(Some(&paths::slug(&default_label)))
        .expect("default workspace just inserted");
    workspaces.set_default(&default_ws.workspace_id);
    if let Err(e) = workspaces::save(&default_ws) {
        tracing::warn!(error = %e, "could not persist default workspace toml");
    } else {
        tracing::info!(
            workspace_id = %default_ws.workspace_id,
            slug = %default_ws.slug,
            "default workspace ready"
        );
    }

    // ADR 0042 slice L1a: adopt every REGISTERED capsule workspace's
    // supervisor on this daemon's own startup — the counterpart of the
    // tmux-session ensure block just above, for the OTHER runtime. This
    // naturally covers the default workspace too when it is a capsule
    // (just marked/preserved above): one registry pass, one code path,
    // no separate "spawn the default's own supervisor" step. `sot-capsule
    // supervise --resume` decides adopt-vs-spawn itself (ADR 0041's
    // start-mode table), including for a workspace whose state directory
    // does not exist yet at all ("no leg at all -> spawn a new leg").
    // Codex review finding 10: runs OFF the startup critical path (a
    // detached task, never awaited) with its own bounded concurrency —
    // see `capsule_workspace::resume_all`'s own doc. Gated to Windows and
    // Linux only (ADR 0043 decision 22): on any other host
    // `workspace.create` never marks a workspace `"capsule"` (see
    // `handlers.rs`), so there is nothing to resume there. On Linux this
    // now finds the same kind of candidates it always found on Windows —
    // every NEW workspace defaults to `"capsule"` there too (ADR 0042
    // L6 / this repo's B6 lane) — plus any surviving `"tmux"` row from
    // before the flip, which this scan still ignores exactly as before.
    #[cfg(any(windows, target_os = "linux"))]
    if let Some(state_root) = sot_log::state_dir::sot_state_dir() {
        tokio::spawn(crate::capsule_workspace::resume_all(state_root, workspaces.clone()));
    } else {
        tracing::warn!(
            "capsule workspace resume-scan skipped: could not resolve this machine's state root \
             ({} unset)",
            crate::capsule_workspace::STATE_ROOT_HINT
        );
    }


    // When the backend is launched with `--label`, stamp our identity into
    // `~/.config/sot/sessions/<slug>.toml` so Sessions mode (frontend)
    // can discover us — including direct shell launches that bypassed
    // tmux.create_session. Frontend-managed sections are preserved.
    if let Some(label) = opts.label.as_deref() {
        match crate::session_state::write_backend_identity(
            label,
            &sid,
            files_mode.root_path(),
            opts.socket.as_deref(),
        ) {
            Ok(path) => {
                tracing::info!(toml = ?path, "wrote backend identity toml");
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not write backend identity toml; Sessions mode discovery may need help");
            }
        }
    }

    // Concept-annotation store at `<project_root>/.concept/`. Created lazily
    // on first write — read/list against a missing directory return empty.
    let concept = Arc::new(ConceptStore::new(files_mode.root_path()));

    // Lazily-spawned MathJax sidecar. Cheap to construct (no child process
    // until the first math.render call); cloning the handle is cheap.
    let mathjax = MathJax::new(MathJax::default_script_path());

    // Lazily-spawned Pluto sidecar. One shared Pluto server per backend,
    // preferring 127.0.0.1:1234 (ephemeral fallback when taken — the daemon
    // learns the actual port from the READY line); spawned on the first
    // `pluto.open`.
    let pluto = Pluto::new(Pluto::default_project_dir(), Pluto::default_start_script());

    // Loopback video file server for browser playback (ADR 0018). Bound at
    // startup so `video.open` URLs are immediately reachable. Prefers
    // `video_port()`, falls back to an ephemeral port when it's taken
    // (another user's daemon on a shared host); URLs and the ADR-0035 proxy
    // allowlist follow the ACTUAL port. Serves only video files, 127.0.0.1
    // only. The warn below now fires only when even the ephemeral bind fails
    // — an exhausted-ports / broken-loopback host, not the collision class.
    if let Err(e) = crate::http_serve::spawn(crate::http_serve::video_port()).await {
        tracing::warn!(error = %e, "video http server failed to start; `o` on a video won't work");
    }

    // Loopback static-site server (ADR 0024). Serves ANY on-disk static site —
    // its root is set per-open by the `docs.open` handler to the cursored file's
    // directory — so `W` opens whatever site/page is selected (HTML/CSS/JS/assets/
    // sub-paths) in the OS browser with full fidelity. Same preferred-then-
    // ephemeral bind story as the video server above. 127.0.0.1 only;
    // workspace-agnostic.
    if let Err(e) = crate::site_serve::spawn(crate::site_serve::site_port()).await {
        tracing::warn!(error = %e, "static-site server failed to start; `W` won't work");
    }
    // ADR 0029 Option B: the dedicated-port pool for root-relative sites
    // (an example project's __site etc.). Taken range ports fall back to
    // ephemeral ones; only a failed ephemeral bind shrinks the pool —
    // docs.open reports "slots busy" when none are assignable.
    crate::site_serve::spawn_pool().await;

    // Lazily-spawned Julia kernel — only fires up when first kernel.request
    // op arrives. The Files-mode walker handles the no-Julia case fine on
    // its own, so this stays a feature flag of sorts.
    let kernel = Kernel::new(
        Kernel::default_kernel_project(),
        files_mode.root_path().to_path_buf(),
    );

    // Streamed REPL frame bus (Option B): every eval's frames are fanned out
    // here off the per-workspace REPL supervisor; each connection subscribes
    // and writes a `repl.frame` evt frame (mirror of the agent-relay bus,
    // minus a client→daemon publish leg — the publisher is the supervisor).
    // Created before the per-workspace REPLs so it can be installed into the
    // registry (`set_repl_frame_tx`) and threaded into the legacy singleton.
    let (repl_frame_tx, _repl_frame_rx) = broadcast::channel::<ReplFrameMsg>(256);
    workspaces.set_repl_frame_tx(repl_frame_tx.clone());

    // Persistent REPL — separate Julia child from the kernel so a runaway
    // eval can't take down introspection. Lazy spawn. The singleton is
    // retained for back-compat on the call chain; all ops now route through
    // per-workspace REPLs (which carry their own workspace_id), so this one
    // publishes with `None` as its workspace_id.
    let repl = Repl::new(repl_frame_tx.clone(), None, None);

    // Shared preview.changed bus (2026-07-10 multiwatch): ONE broadcast
    // channel every connection subscribes to, fed by ONE file watcher PER
    // WORKSPACE (spawned at registration — Workspaces::set_watch_bus also
    // catches up any workspace registered before this line). Previously a
    // single watcher covered only the default workspace root, so no other
    // workspace's nav ever live-refreshed (the documented KNOWN GAP).
    // Events carry the workspace slug; each connection filters on it (and on
    // path-under-root) at write time — `preview_changed_visible` below. Not
    // paired with `session` (the ring handle): `preview.changed` no longer
    // touches the session ring at all — see `watcher.rs`'s header comment.
    let (preview_changed_tx, _preview_changed_rx) =
        broadcast::channel::<PreviewChanged>(256);
    workspaces.set_watch_bus(preview_changed_tx.clone());

    // Workspace lifecycle bus: parallel to the file watcher's broadcast, but
    // typed `WorkspaceChanged`. Handlers publish on a successful create/
    // destroy; each connection subscribes and writes a `workspace.changed`
    // evt frame so the Sessions strip refreshes live (mirror preview.changed).
    let (ws_events_tx, _ws_events_rx) = broadcast::channel::<WorkspaceChanged>(64);

    // Topology write path (plan §B "Editing the master list"): one store
    // per daemon holding the last successfully parsed `hosts.toml`, and a
    // broadcast bus parallel to the workspace one, typed `TopologyChanged`.
    // `topology.set` publishes here after a successful write; `version.query`
    // (and every `topology.*` op) re-reads the file on-demand and publishes
    // the same way when it notices a hand edit nobody else already announced
    // — never a file watcher (the file can live on a network filesystem).
    let topology_store = std::sync::Arc::new(crate::topology_store::TopologyStore::new(
        sot_protocol::topology::locate().unwrap_or_else(|| PathBuf::from("hosts.toml")),
    ));
    let (topo_changed_tx, _topo_changed_rx) = broadcast::channel::<crate::topology_store::TopologyChanged>(16);

    // ADE state-nav live refresh: poll the sot-comm registry and publish a
    // `workspace.changed` whenever an agent's work-state actually changes, so the
    // Sessions strip re-lists LIVE (the FE re-issues workspace.list on the evt).
    // POLL, not notify — the registry is on NFS where inotify is unreliable; the
    // 1.5s tick also coalesces a working agent's periodic status_at re-stamps. The
    // diff excludes `last_seen` so frequent send/poll heartbeats never spam re-lists.
    if let Some(reg_path) = crate::handlers::comm_registry_path() {
        let tx = ws_events_tx.clone();
        tokio::spawn(async move {
            let mut last: Option<String> = None;
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(1500));
            loop {
                tick.tick().await;
                if let Some(cur) = tokio::fs::read(&reg_path)
                    .await
                    .ok()
                    .map(|b| project_comm_registry(&b))
                {
                    if last.as_ref().map_or(false, |p| *p != cur) {
                        let _ = tx.send(WorkspaceChanged {
                            action: "agent_state".into(),
                            slug: String::new(),
                            workspace_id: String::new(),
                        });
                    }
                    last = Some(cur);
                }
            }
        });
    }


    // Agent-relay bus: parallel to the workspace bus, typed `AgentMessage`.
    // `agent.send` publishes here; each connection subscribes and writes an
    // `agent.message` evt frame so a message reaches the other machine's
    // in-terminal agent instantly over the SSH-forwarded socket (mirror of
    // the workspace.changed wiring, plus a client→daemon publish leg).
    let (agent_events_tx, _agent_events_rx) = broadcast::channel::<AgentMessage>(256);

    // FE-command bus (ADR 0025): parallel to the agent-relay bus, typed
    // `FeCommandEvt`. `fe.command.send` publishes here; each connection
    // subscribes and writes an `fe.command` evt frame so an imperative UI
    // command (preview/reveal/goto/notify) reaches every connected frontend
    // instantly over the SSH-forwarded socket. Mirror of the agent_events_tx
    // wiring, plus the same client→daemon publish leg. Broadcast to ALL
    // connections; the FE self-filters on `target`.
    let (fe_command_tx, _fe_command_rx) = broadcast::channel::<FeCommandEvt>(256);

    // Server-monitoring data plane (ADR 0020): always-on samplers (one per
    // host) feeding a tiered ring + the `monitor.tick` broadcast bus. Stored on
    // the registry (mirrors `set_repl_frame_tx`) so every connection can
    // subscribe and the `monitor.*` ops can reach it. Sampling runs for the
    // life of the backend so the drawer shows real history the moment it opens;
    // per-connection tick delivery is gated by `monitor.subscribe`.
    let monitor_hub = crate::monitor::MonitorHub::start(crate::monitor::load_hosts());
    workspaces.set_monitor_hub(monitor_hub);

    // Connected-frontend registry (ADR 0010/0013 multi-frontend). Shared
    // across both listeners so the live count spans transports; each
    // connection registers on hello and deregisters on drop.
    let clients = Clients::new();

    // Auto-updater (ADR 0030 §4, Phase C): daily check that, on a newer
    // release, pushes an `fe.command` `notify` over the bus above and runs
    // the stage → prepare → arm pipeline. In `auto` mode it may also exit
    // for the apply owner when no clients are attached (hence the roster
    // handle). A `-dev` build (the whole fleet) disables it at the hard
    // guard inside `spawn_periodic`, which logs the disabled state and
    // returns without spawning anything.
    crate::update::spawn_periodic(fe_command_tx.clone(), clients.clone());

    let label = Arc::new(opts.label);
    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    if let Some(path) = opts.socket {
        let s = session.clone();
        let tok = Arc::new(None);
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = preview_changed_tx.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        let tps = topology_store.clone();
        let tpe = topo_changed_tx.clone();
        tasks.push(tokio::spawn(async move {
            run_local(
                path, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, fce, rfe, cl, tps, tpe,
            )
            .await
        }));
    }

    if tasks.is_empty() {
        anyhow::bail!("no listener configured");
    }

    // Wait for whichever listener errors first; on a clean run they loop
    // forever, so this only returns on a real failure.
    let (res, _idx, _rest) = futures_util::future::select_all(tasks).await;
    res.context("listener task panicked")??;
    Ok(())
}

async fn run_local(
    socket_path: PathBuf,
    session: Session,
    token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / handle_connection signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    preview_changed_tx: broadcast::Sender<PreviewChanged>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    topology_store: Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology_store::TopologyChanged>,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() {
            paths::secure_socket_dir(parent)
                .with_context(|| format!("secure socket dir {}", parent.display()))?;
        }
    }
    // Unix sockets leave a filesystem entry that blocks rebind; Windows
    // named pipes don't, so only do the cleanup on Unix.
    #[cfg(unix)]
    if std::path::Path::new(&socket_path).exists() {
        tokio::fs::remove_file(&socket_path)
            .await
            .with_context(|| format!("remove stale socket {socket_path:?}"))?;
    }

    let path_str = socket_path
        .to_str()
        .context("socket path must be valid UTF-8")?;
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .with_context(|| format!("bind {socket_path:?}"))?;
    tracing::info!(socket = ?socket_path, "listening (local)");

    loop {
        let stream: LocalStream = listener.accept().await.context("accept on sot socket")?;
        let s = session.clone();
        let tok = token.clone();
        let mj = mathjax.clone();
        let pl = pluto.clone();
        let fm = files_mode.clone();
        let ke = kernel.clone();
        let co = concept.clone();
        let rp = repl.clone();
        let wa = preview_changed_tx.clone();
        let lb = label.clone();
        let ws = workspaces.clone();
        let wse = ws_events_tx.clone();
        let age = agent_events_tx.clone();
        let fce = fe_command_tx.clone();
        let rfe = repl_frame_tx.clone();
        let cl = clients.clone();
        let tps = topology_store.clone();
        let tpe = topo_changed_tx.clone();
        tokio::spawn(async move {
            let (rx, tx) = stream.split();
            if let Err(e) = handle_connection(
                rx, tx, s, tok, mj, pl, fm, ke, co, rp, wa, lb, ws, wse, age, fce, rfe, cl,
                tps, tpe, "local", None,
            )
            .await
            {
                tracing::warn!(error = %e, transport = "local", "connection ended with error");
            } else {
                tracing::info!(transport = "local", "connection closed");
            }
        });
    }
}


/// Read one frame while *owning* the buffered reader, handing it back with
/// the result. Lets the per-connection select! loop keep a single in-flight
/// read future across iterations so a cancelled select! *pauses* a mid-blob
/// read rather than dropping it — `codec::read_frame` is NOT cancellation-safe
/// (it reads the `\n` envelope then `read_exact`s the blob tail across two
/// awaits). Mirrors the frontend transport fix (commit 8746b74). See the
/// CANCEL-SAFETY note in `handle_connection`'s loop.
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// Generic over the AsyncRead/AsyncWrite halves (a relic of the two-transport
/// era that keeps this testable against in-memory duplex streams).
/// `expected_token` is always `None` since 0.4.0 removed the TCP listener —
/// the local socket's access check happened at the socket path; the param and
/// the hello `token` wire field survive for cross-version compat.
/// A request whose handler held the connection loop at least this long is
/// logged at info with its op and service time (see the dispatch timer in
/// `handle_connection`). 50 ms is well above any cheap op and well below a
/// switch the user can feel.
const SLOW_REQUEST_MS: u64 = 50;

/// Stamp this connection's `last_person_input_at` if it has registered.
/// Call ONLY from the `fe.presence` op arm (2026-09-08 review rework,
/// design point A) — that op alone is trustworthy evidence of a person,
/// because the frontend sends it from its own real keyboard/mouse input
/// handlers, throttled there, never from a command-file or other
/// automated path. An EARLIER design stamped this from ordinary
/// navigation ops (`tree.root`, `preview.get`, `pty.write`, a
/// `workspace.activate` with `read: true`); review found every one of
/// them had an automated producer too (reconnect re-announces tree/preview
/// requests, a badge-consuming `goto` fires them, an autostarted agent
/// writes into its own pane, and a command-file `cycle_ws` could forge
/// `read: true`) — deleted rather than patched. A no-op pre-hello, when
/// `client_guard` is still `None`.
fn touch_person_input(clients: &Clients, guard: &Option<ClientGuard>) {
    if let Some(g) = guard {
        clients.touch_person_input(g.serial());
    }
}

async fn handle_connection<R, W>(
    rx: R,
    mut tx: W,
    session: Session,
    expected_token: Arc<Option<String>>,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    // Singleton handles retained on the call chain for backward compat
    // and to keep the run_local / handle_connection signatures unchanged. All op
    // handlers now route through Workspaces per ADR 0014; these
    // bindings are dead in `handle_connection` itself.
    #[allow(unused_variables, dead_code)] kernel: Kernel,
    #[allow(unused_variables, dead_code)] concept: Arc<ConceptStore>,
    #[allow(unused_variables, dead_code)] repl: Repl,
    preview_changed_tx: broadcast::Sender<PreviewChanged>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    topology_store: Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology_store::TopologyChanged>,
    transport: &'static str,
    peer: Option<String>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // CANCEL-SAFETY: `read_frame` is NOT cancellation-safe (it reads the `\n`
    // envelope then `read_exact`s the blob tail across two awaits). Polling
    // `codec::read_frame(&mut rx)` directly as a select! arm meant that when
    // another arm (pty bytes / watcher change) completed mid-blob, select!
    // dropped the half-read future: the envelope was consumed but the blob
    // tail was not, so the next read parsed leftover binary as a JSON envelope
    // and failed, dropping the connection. Hold one read future across loop
    // iterations and poll it by `&mut`, so a cancelled select! pauses it (it
    // resumes mid-blob next iteration). The future owns the reader (via
    // `read_owned`) and hands it back on completion. Mirrors frontend 8746b74;
    // lower-risk here (frontend→backend reqs rarely carry blob tails) but the
    // same latent footgun. Flagged from the Windows side 2026-05-26.
    let mut buffered = codec::buffered(rx);

    // ADR 0035: a proxy connection announces itself with `proxy.connect` as
    // its VERY FIRST frame and then becomes a raw byte pipe — it must never
    // enter the multiplexed control loop below (where it would be
    // hello-gated and could head-of-line-block pty/repl traffic). Peek that
    // one frame here, before the persistent read future is armed. On
    // proxy.connect, hand the (buffered) reader + write half straight to the
    // pipe and return. Otherwise, remember the frame and feed it to the loop
    // as its first dispatched frame (`pending_first`), so the peek costs the
    // control path nothing.
    let mut pending_first: Option<(Frame, Option<Vec<u8>>)> = match codec::read_frame(&mut buffered)
        .await
    {
        Ok((f, blob)) => {
            if f.kind == Kind::Req && f.op == op::PROXY_CONNECT {
                tracing::info!(transport, "proxy.connect — leaving control loop for a raw pipe");
                return crate::proxy::handle_proxy_connect(
                    buffered,
                    tx,
                    f,
                    expected_token.as_deref(),
                )
                .await;
            }
            // ADR 0045 decision 2: capsule-runtime-gated exactly like
            // `lane_bridge.rs` itself — on a host with no capsule runtime
            // at all (macOS), `lane.connect` is not specially peeked and
            // falls into the ordinary control loop below, which answers
            // whatever "unknown op" every other unrouted op string
            // already does.
            #[cfg(any(windows, target_os = "linux"))]
            if f.kind == Kind::Req && f.op == op::LANE_CONNECT {
                tracing::info!(transport, "lane.connect — leaving control loop for a raw pipe");
                return crate::lane_bridge::handle_lane_connect(
                    buffered,
                    tx,
                    f,
                    expected_token.as_deref(),
                    &workspaces,
                )
                .await;
            }
            Some((f, blob))
        }
        Err(e) => {
            tracing::debug!(error = %e, transport, "first read failed before any frame; closing");
            return Ok(());
        }
    };

    let mut read_fut = Some(Box::pin(read_owned(buffered)));
    tracing::debug!(transport, "connection ready");


    // Connected-client registry entry (ADR 0010/0013). Registered on the
    // first `hello` (when this connection's client_id is known) and held
    // for the connection's lifetime; the guard deregisters on any exit
    // path (clean EOF, error, task drop). `None` until hello arrives.
    let mut client_guard: Option<crate::clients::ClientGuard> = None;
    // This connection's own declared host (`HelloReq.host`, ADR 0046
    // decision 1), captured at hello — `topology.set`'s "can't remove
    // yourself" refusal reads it (server.rs, `op::TOPOLOGY_SET`).
    let mut hello_host: Option<String> = None;

    // Half-open long-lived-role reaper (topology plan §F step 2). A
    // tunnelled `fe`/`bridge` connection reaches this daemon as sshd's
    // Unix-socket side, so `WRITE_TIMEOUT` above never fires for it — a
    // half-open peer's death is otherwise noticed only when the OS-level
    // TCP side gives up (15 min to 2 h). `is_long_lived_role` flips true on
    // hello, exactly when this connection's declared role resolves to
    // `fe`/`bridge` (`cli`/`agent`, one-shot roles, stay false forever and
    // are never deadline-gated) — it is only ELIGIBILITY, not the gate
    // itself.
    //
    // Opt-in by ping (manager compatibility fix, post-review): the gate is
    // `deadline_armed`, which flips true only on this connection's FIRST
    // `ping` — never at hello. An `fe`/`bridge` peer too old to send ping
    // (a frontend box or a comm bridge that hasn't converged from main
    // yet) is eligible but never arms, so it keeps TODAY's behaviour
    // exactly: never reaped by this path. Arming at hello instead would
    // have dropped every such peer every 90s forever (endless reconnect
    // churn, a message-loss window each cycle) — the stale-roster defect
    // this step fixes then persists only for clients too old to ping,
    // which is correct and self-healing as they upgrade.
    //
    // Once armed, `read_deadline` is a fixed point in time — bumped
    // forward to `now + ping_read_deadline()` every time ANY frame is read
    // from this connection (not only `ping`; ordinary traffic is just as
    // much proof of life), never recomputed relative to "now" at each
    // select! poll, so re-entering select! every loop iteration doesn't
    // itself push it out. The initial `Instant::now()` here is never
    // acted on: the corresponding select! arm and the bump below are both
    // `deadline_armed`-gated, and that starts false.
    let mut is_long_lived_role = false;
    let mut deadline_armed = false;
    let mut read_deadline = tokio::time::Instant::now();

    // Per-connection auth state (ADR 0010 hardening). The token gate on `hello`
    // is not sufficient on its own: nothing forces a client to send hello, and
    // the dispatch loop below serves file.read / repl.eval / file.download /
    // agent.send with no handshake — so a token-configured backend was still
    // fully reachable by simply skipping hello. Starts `true` ONLY in open-config
    // mode (no token configured); when a token IS configured it starts `false`
    // and flips to `true` only on a hello whose token matches. Every non-hello
    // op is gated on this flag.
    let mut authenticated = expected_token.is_none();

    // This connection's active workspace, made EXPLICIT via `workspace.activate`
    // (`op::WORKSPACE_ACTIVATE`) — the frontend's single "switch chrome" entry
    // point (`switch_to_workspace`, gpu.rs) fires it UNCONDITIONALLY as the
    // very first wire action of any switch, including a UI-cache hit that
    // fires no other request at all, and a switch back to the default
    // workspace. Also fired once on reconnect, right after `hello` succeeds.
    // A prior revision of this fix INFERRED the active workspace from
    // whichever op's `workspace_id` happened to arrive next — dropped
    // (Codex review): a cache hit fired no op, so the daemon could sit on a
    // stale workspace indefinitely, and a client reconnect's initial
    // default-workspace fetch could clobber a resumed non-default workspace
    // before the frontend got around to re-requesting it.
    //
    // Stores ONLY the canonical `workspace_id` (never a cached slug/root
    // tuple): resolving fresh at every use (`preview_changed_visible`) is
    // what makes a same-slug reinsertion, an uncanonical stored root, or a
    // stale slug match after a destroy-then-recreate impossible to leak —
    // there is no cache left to go stale.
    //
    // `None` = never activated (a fresh connection, before its first
    // `workspace.activate`) — keeps seeing every `preview.changed` event,
    // exactly as before this filter existed. `Some(id)` where `id` no longer
    // RESOLVES (the workspace was destroyed since activation, or the
    // activate itself named something that never resolved) is a DIFFERENT
    // state — `preview_changed_visible` drops every event in that case
    // rather than falling back to "send everything": the connection told us
    // it was viewing a specific workspace, so there is no view left to serve
    // traffic to until the next successful activate.
    let mut active_workspace: Option<String> = None;

    // One file-watcher subscription per connection. Each connection writes
    // its own preview.changed evt frame; the broadcast channel's per-
    // receiver lag detection lets us notice and log if this connection is
    // falling behind editor saves.
    let mut watcher_rx = Some(preview_changed_tx.subscribe());

    // One workspace-lifecycle subscription per connection. Always present
    // (the channel is created unconditionally in `run`), unlike the file
    // watcher which can fail to start. Each connection writes its own
    // `workspace.changed` evt frame; the broadcast's per-receiver lag
    // detection surfaces a connection that fell behind.
    let mut ws_events_rx = ws_events_tx.subscribe();

    // One topology subscription per connection, same shape as the
    // workspace-lifecycle bus above (plan §B): each connection writes its
    // own `topology.changed` evt frame so Hosts mode refreshes live.
    let mut topo_changed_rx = topo_changed_tx.subscribe();

    // One agent-relay subscription per connection. Like the workspace bus
    // it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `agent.message` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut agent_events_rx = agent_events_tx.subscribe();

    // One FE-command subscription per connection (ADR 0025). Like the agent
    // bus it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `fe.command` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut fe_command_rx = fe_command_tx.subscribe();

    // One REPL-frame subscription per connection. Streamed eval frames arrive
    // here (published by the per-workspace REPL supervisor); each connection
    // writes its own `repl.frame` evt frame. Like the agent bus, always
    // present; the broadcast's per-receiver lag detection surfaces a
    // connection that fell behind a fast-printing eval.
    let mut repl_frame_rx = repl_frame_tx.subscribe();

    // Monitor tick bus (ADR 0020). Sampling is always-on; this connection only
    // forwards ticks while its drawer is open — `monitor.subscribe` flips the
    // flag, `monitor.unsubscribe` clears it. The receiver is still polled while
    // unsubscribed so it never lags (the client gets fresh ticks on subscribe).
    // Optional like `watcher_rx`: `None` if the hub wasn't installed.
    let mut monitor_rx = workspaces.monitor_hub().map(|h| h.subscribe());
    let mut monitor_subscribed = false;

    // Off-loop jobs (switch-latency Phase 1, joined by `kernel.request` in
    // the kernel-dead-pane-starvation fix): `preview.get` / `concept.read`
    // / `image.crop` / `kernel.request` run as their own tasks in `jobs`,
    // bounded to `OFFLOOP_CONCURRENCY` concurrent (the semaphore is acquired INSIDE
    // each job, never here, so queuing one never blocks reading the next
    // frame). `tx` stays this loop's alone — a job has no way to reach the
    // socket, so it hands its finished reply to `out_tx` and the loop
    // (`out_rx`, drained in the select below) writes it exactly like an
    // inline reply. Dropping `jobs` (this function returning, any path)
    // aborts whatever's still running — no leaked tasks.
    let (out_tx, mut out_rx): (OutTx, _) = mpsc::channel(OFFLOOP_CONCURRENCY);
    let mut jobs: JoinSet<()> = JoinSet::new();
    let job_sem = Arc::new(Semaphore::new(OFFLOOP_CONCURRENCY));

    loop {
        // ADR 0035: the peeked first frame (any non-proxy first frame, e.g.
        // hello) is dispatched here before the first socket read, so the
        // proxy detection above costs the control path nothing.
        let frame = if let Some((f, _blob)) = pending_first.take() {
            f
        } else {
            tokio::select! {
                biased;
                Some((frame, blob)) = out_rx.recv() => {
                    write_reply(&mut tx, frame, blob).await?;
                    continue;
                }
                done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                    let (rx_back, wire) = done;
                    read_fut = Some(Box::pin(read_owned(rx_back)));
                    match wire {
                        Ok((f, _blob)) => f,
                        Err(e) => {
                            tracing::debug!(error = %e, transport, "read_frame returned; closing");
                            return Ok(());
                        }
                    }
                }
                change = recv_watcher(&mut watcher_rx) => {
                    if authenticated {
                        write_preview_changed(
                            &mut tx,
                            change,
                            transport,
                            active_workspace.as_deref(),
                            &workspaces,
                        )
                        .await?;
                    }
                    continue;
                }
                wsc = recv_ws_events(&mut ws_events_rx) => {
                    // Auth gate (ADR 0010 hardening): never push evt frames to an
                    // unauthenticated connection. Drain the channel, drop the frame.
                    if authenticated {
                        write_workspace_changed(&mut tx, wsc, transport).await?;
                    }
                    continue;
                }
                tpc = recv_topo_changed(&mut topo_changed_rx) => {
                    if authenticated {
                        write_topology_changed(&mut tx, tpc, transport).await?;
                    }
                    continue;
                }
                msg = recv_agent_msg(&mut agent_events_rx) => {
                    if authenticated {
                        write_agent_message(&mut tx, msg, transport).await?;
                    }
                    continue;
                }
                fc = recv_fe_command(&mut fe_command_rx) => {
                    if authenticated {
                        write_fe_command(&mut tx, fc, transport, client_guard.as_ref().map(|g| g.serial())).await?;
                    }
                    continue;
                }
                rf = recv_repl_frame(&mut repl_frame_rx) => {
                    if authenticated {
                        write_repl_frame(&mut tx, rf, transport).await?;
                    }
                    continue;
                }
                tick = recv_monitor(&mut monitor_rx) => {
                    if authenticated && monitor_subscribed {
                        write_monitor_tick(&mut tx, tick, transport).await?;
                    }
                    continue;
                }
                // Same hygiene drain as the pty-present arm above.
                Some(res) = jobs.join_next(), if !jobs.is_empty() => {
                    if let Err(e) = res {
                        tracing::error!(error = %e, transport, "off-loop job panicked");
                    }
                    continue;
                }
                // Topology plan §F step 2: an ARMED `fe`/`bridge` connection
                // (has sent at least one `ping`) that has since sent no
                // frame at all within `ping_read_deadline()` is dead — reap
                // it exactly like a clean EOF. Guarded on `deadline_armed`,
                // not merely `is_long_lived_role`, so this arm stays inert
                // — never even polled — before hello, for `cli`/`agent`
                // connections, AND for an `fe`/`bridge` peer that has never
                // sent a `ping` at all (opt-in by ping: see the arming
                // comment above `is_long_lived_role`'s declaration).
                () = tokio::time::sleep_until(read_deadline), if deadline_armed => {
                    tracing::info!(transport, ?peer, "no frame within the read deadline; reaping half-open connection");
                    return Ok(());
                }
            }
        };

        // Any frame from an ARMED connection is proof of life — push the
        // reaper deadline back out (topology plan §F step 2). Deliberately
        // unconditional on the op: ordinary traffic counts exactly as much
        // as a `ping`, so a busy connection never needs one. Before this
        // connection's first `ping` (`deadline_armed` still false) this is
        // a no-op; arming itself (flag + first deadline) happens in the
        // `op::PING` arm below, the moment role-eligibility (set at hello)
        // and a first ping coincide.
        if deadline_armed {
            read_deadline = tokio::time::Instant::now() + ping_read_deadline();
        }

        if frame.kind != Kind::Req {
            tracing::debug!(?frame.kind, op = %frame.op, transport, "ignoring non-req frame");
            continue;
        }

        // Auth gate (ADR 0010 hardening). With a token configured, every op
        // except `hello` requires a prior token-valid hello on THIS connection.
        // Without this, the token is trivially bypassable: a client skips the
        // handshake and calls file.read / repl.eval / file.download / agent.send
        // directly, and the dispatch loop below serves them regardless.
        if !authenticated && frame.op.as_str() != op::HELLO {
            tracing::warn!(op = %frame.op, ?peer, "op rejected: unauthenticated (no token-valid hello)");
            let payload = serde_json::json!({
                "error": "authentication required: send a token-valid hello first",
                "code": "unauthenticated",
            });
            write_frame_to(&mut tx, &Frame::res(frame.id, &frame.op, payload), None).await?;
            continue;
        }

        // Each arm evaluates to `Result<HandlerOutput>`; the containment block
        // after the match turns a handler `Err` into an error *frame* for this
        // request instead of letting it bubble out of `handle_connection` and
        // tear down the whole connection (pre-fix, one malformed payload for
        // any op dropped the socket and forced a full FE reconnect).
        // Per-request service time (switch-latency Phase 1, 2026-09-08): the
        // loop below awaits every handler inline, so one slow request delays
        // every later frame on this connection. Logged at info above
        // SLOW_REQUEST_MS so the culprit op is identified, never inferred
        // from a neighbouring log line.
        let dispatch_started = std::time::Instant::now();
        let dispatched: Result<handlers::HandlerOutput> = match frame.op.as_str() {
            op::HELLO => {
                // Register this connection in the client roster the first
                // time we learn its client_id (a reconnect re-sends hello
                // on the same connection — keep the original guard). Done
                // before `handle_hello` so `clients_connected` counts self.
                if client_guard.is_none() {
                    if let Ok(req) =
                        serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
                    {
                        // Topology plan §F step 2: mark this connection
                        // ELIGIBLE for the read-deadline reaper -- exactly
                        // the two long-lived roles, `fe` and `bridge`
                        // (`cli`/`agent` are one-shot and stay ungated).
                        // This does NOT arm the deadline itself (manager
                        // compatibility fix, post-review) — only this
                        // connection's FIRST `ping` does that (`op::PING`
                        // arm below), so a peer too old to send one keeps
                        // today's behaviour exactly, never reaped by this
                        // path.
                        // A peer on another protocol is about to be
                        // refused by `handle_hello`'s gate: never enter
                        // the roster (it would be counted as a directed
                        // command's audience and listed by `version.query`
                        // while its hello stands refused). It gets the
                        // structured mismatch reply and nothing else.
                        if req.protocol == sot_protocol::PROTOCOL_VERSION {
                            is_long_lived_role = matches!(req.role.as_str(), "fe" | "bridge");
                            hello_host = req.host.clone();
                            client_guard = Some(clients.register(
                                req.client_id,
                                transport,
                                peer.clone(),
                                req.app_version,
                                req.protocol,
                                req.role,
                                req.host,
                                req.instance,
                                req.name,
                            ));
                        }
                    }
                }
                // Flip the per-connection auth flag based on THIS hello's token
                // (recomputed on every hello so a reconnect re-auths). Open-config
                // mode has `expected_token == None`, so this stays true. Mirrors
                // the same check `handle_hello` uses to shape its response frame.
                authenticated = match expected_token.as_deref() {
                    None => true,
                    Some(expected) => {
                        let presented =
                            serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
                                .ok()
                                .and_then(|r| r.token)
                                .unwrap_or_default();
                        handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes())
                    }
                };
                handlers::handle_hello(
                    frame.id,
                    frame.payload,
                    &session,
                    expected_token.as_ref(),
                    &files_mode,
                    label.as_deref(),
                    &clients,
                )
                .await
            }
            op::TREE_ROOT => {
                handlers::handle_tree_root(frame.id, frame.payload, &session, &workspaces).await
            }
            op::TREE_CHILDREN => {
                handlers::handle_tree_children(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::NAV_TOGGLE_HIDDEN => {
                handlers::handle_nav_toggle_hidden(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::PREVIEW_GET => {
                // Off-loop (switch-latency Phase 1): a read-and-render of
                // the requested node. `preview.set_scale` stays INLINE just
                // below — it writes a `.scale.json` sidecar.
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    transport,
                    async move {
                        handlers::handle_preview_get(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::PREVIEW_SET_SCALE => {
                handlers::handle_preview_set_scale(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::IMAGE_CROP => {
                // Off-loop (switch-latency Phase 1): decodes the source
                // image and writes a NEW, uniquely-named capture file — it
                // never mutates any EXISTING shared state (the session
                // revision bump it also does is safe off-loop for the same
                // reason concurrent connections already interleave those
                // bumps: their order relative to wall-clock request order
                // was never guaranteed).
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    transport,
                    async move {
                        handlers::handle_image_crop(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::MATH_RENDER => {
                handlers::handle_math_render(frame.id, frame.payload, &session, &mathjax).await
            }
            op::PLUTO_OPEN => {
                handlers::handle_pluto_open(frame.id, frame.payload, &session, &pluto, &workspaces)
                    .await
            }
            op::VIDEO_OPEN => {
                handlers::handle_video_open(frame.id, frame.payload, &session).await
            }
            op::DOCS_OPEN => {
                // Per-connection site root (ADR 0029): this connection's serial
                // selects/owns its docs-map entry and becomes the URL's first path
                // segment. `None` only before hello registers the guard, which
                // always precedes docs.open in practice.
                let serial = client_guard.as_ref().map(|g| g.serial());
                handlers::handle_docs_open(frame.id, frame.payload, &session, serial, &workspaces)
                    .await
            }
            op::QUARTO_OPEN => {
                handlers::handle_quarto_open(frame.id, frame.payload, &session).await
            }
            op::FILE_UPLOAD => handlers::handle_file_upload(frame.id, frame.payload).await,
            op::FILE_DOWNLOAD => {
                // Streams chunk frames straight to the socket (bounded memory),
                // so it writes its own frames and skips the response-write below.
                handlers::stream_file_download(&mut tx, frame.id, frame.payload).await?;
                continue;
            }
            op::KERNEL_REQUEST => {
                // Off-loop: this op used to await
                // `handlers::handle_kernel_request(...)` INLINE, in this
                // same per-connection dispatch loop that
                // also carries this connection's `pty` byte stream — a
                // `kernel.request` against a dead/slow kernel held up
                // dispatch of the NEXT frame on this connection, including
                // a `pty.write`/`pty.open` for the same session's attached
                // pane (the frontend multiplexes both over one connection
                // per host). `preview.get`/`concept.read`/`image.crop` were
                // already off-loop for the identical reason; this joins
                // their existing pool (`job_sem`, `OFFLOOP_CONCURRENCY`)
                // rather than adding a second cap for the same shape of
                // operation (a bounded external-process call). Note this is
                // NOT about `job_sem` ever being shared with pty ops —
                // pty.* dispatch inline just below and never touch it; the
                // actual shared choke point was the inline `.await` itself.
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    transport,
                    async move {
                        handlers::handle_kernel_request(req_id, payload, &session, &workspaces)
                            .await
                    },
                );
                continue;
            }
            op::CONCEPT_READ => {
                // Off-loop (switch-latency Phase 1): a read of one
                // `.concept/` annotation file. `concept.write`/`concept.list`
                // stay INLINE (write, and directory-walk-then-read).
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    transport,
                    async move {
                        // Test-only (see `test_slow_concept_read_delay`): a
                        // no-op sleep unless a test set the env var.
                        let delay = test_slow_concept_read_delay();
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        handlers::handle_concept_read(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::CONCEPT_WRITE => {
                handlers::handle_concept_write(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::CONCEPT_LIST => {
                handlers::handle_concept_list(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::FILE_READ => {
                handlers::handle_file_read(frame.id, frame.payload, &session, &workspaces).await
            }
            op::FILE_WRITE => {
                handlers::handle_file_write(frame.id, frame.payload, &session, &workspaces).await
            }
            op::FILE_DELETE => {
                handlers::handle_file_delete(frame.id, frame.payload, &session, &workspaces).await
            }
            op::DIR_CREATE => {
                handlers::handle_dir_create(frame.id, frame.payload, &session, &workspaces).await
            }
            op::REPL_EVAL => {
                handlers::handle_repl_eval(frame.id, frame.payload, &session, &workspaces).await
            }
            op::REPL_RUN_FILE => {
                handlers::handle_repl_run_file(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::REPL_INTERRUPT => {
                handlers::handle_repl_interrupt(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::REPL_EXECUTE => {
                handlers::handle_repl_execute(frame.id, frame.payload, &session, &workspaces).await
            }
            op::DIRECTORY_LIST => {
                handlers::handle_directory_list(frame.id, frame.payload, &session).await
            }
            op::WORKSPACE_CREATE => {
                handlers::handle_workspace_create(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await
            }
            op::WORKSPACE_LIST => {
                handlers::handle_workspace_list(frame.id, frame.payload, &workspaces).await
            }
            op::WORKSPACE_ACTIVATE => {
                // Update `active_workspace` (declared above) HERE, inline —
                // same pattern as HELLO's auth flag just above: peek the raw
                // JSON for `workspace_id` before the typed parse the handler
                // does again, so a malformed payload still reaches the
                // handler's ordinary error path instead of silently
                // skipping the state update.
                //
                // Store the RESOLVED canonical id when it resolves. When it
                // doesn't (a stale id, or a race with a concurrent destroy),
                // record the raw hint verbatim rather than leaving the
                // PREVIOUS activation in place — the frontend just told us
                // its view moved off that workspace, so continuing to
                // filter by the stale one would leak the old view's events
                // into the new one. `preview_changed_visible` re-resolves at
                // write time and drops everything for an id that still
                // doesn't resolve then.
                let hinted = frame.payload.get("workspace_id").and_then(|v| v.as_str());
                active_workspace = Some(
                    workspaces
                        .resolve(hinted)
                        .map(|ws| ws.workspace_id.clone())
                        .unwrap_or_else(|| hinted.unwrap_or_default().to_string()),
                );
                handlers::handle_workspace_activate(frame.id, frame.payload, &workspaces).await
            }
            op::AGENT_SEND => {
                handlers::handle_agent_send(frame.id, frame.payload, &agent_events_tx).await
            }
            op::AGENT_JOIN => {
                handlers::handle_agent_join(frame.id, frame.payload, &workspaces, &ws_events_tx)
                    .await
            }
            op::FE_COMMAND_SEND => {
                handlers::handle_fe_command_send(frame.id, frame.payload, &fe_command_tx, &clients)
                    .await
            }
            op::FE_PRESENCE => {
                // The ONLY place `last_person_input_at` is stamped from
                // (2026-09-08 review rework, design point A) — see
                // `touch_person_input`'s doc for why every other op that
                // used to stamp it was removed instead of patched.
                touch_person_input(&clients, &client_guard);
                handlers::handle_fe_presence(frame.id).await
            }
            op::PING => {
                // Opt-in arming (manager compatibility fix, post-review):
                // this connection's FIRST `ping`, and only if hello already
                // marked it role-eligible, arms the read-deadline reaper --
                // never hello itself. A peer that never pings (an old
                // frontend or comm bridge not yet converged from main)
                // stays permanently unarmed and keeps today's behaviour:
                // never reaped by this path. Once armed, the generic bump
                // above keeps pushing `read_deadline` out on every
                // subsequent frame, `ping` included.
                if is_long_lived_role && !deadline_armed {
                    deadline_armed = true;
                    read_deadline = tokio::time::Instant::now() + ping_read_deadline();
                }
                handlers::handle_ping(frame.id).await
            }
            op::UPDATE_CHECK => crate::update::handle_update_check(frame.id).await,
            op::UPDATE_APPLY => {
                crate::update::handle_update_apply(frame.id, &fe_command_tx).await
            }
            op::VERSION_QUERY => {
                handlers::handle_version_query(frame.id, &clients, &topology_store, &topo_changed_tx)
                    .await
            }
            op::TOPOLOGY_SET => {
                crate::topology_set::handle_topology_set(
                    frame.id,
                    frame.payload,
                    &topology_store,
                    &workspaces,
                    &crate::workspaces::declared_host(),
                    hello_host.as_deref(),
                    &topo_changed_tx,
                )
                .await
            }
            op::WORKSPACE_DESTROY => {
                handlers::handle_workspace_destroy(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await
            }
            op::PTY_OPEN => {
                let req: PtyOpenReq = match serde_json::from_value(frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let payload = serde_json::json!({
                            "error": format!("pty.open payload: {e}"),
                            "code": "bad_request",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                };
                // Name validation (security review): an explicit `target`
                // becomes the real tmux session name — a `|`-containing one
                // would corrupt `tmux.rs`'s naive `|`-delimited
                // `list-sessions`/`list-panes` parsing for every session, not
                // just this one. `None` (the default target) is exempt: it's
                // the hardcoded `DEFAULT_TMUX_TARGET` constant, not
                // request-controlled.
                if let Some(t) = req.target.as_deref() {
                    if !handlers::valid_name(t) {
                        let payload = serde_json::json!({
                            "error": format!(
                                "invalid target {t:?} (want 1-64 chars of [A-Za-z0-9._-])"
                            ),
                            "code": "bad_target",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                }
                let requested_target = req
                    .target
                    .as_deref()
                    .unwrap_or("");
                // A row's agent pane is a capsule (ADR 0046): `pty.open`
                // starts its supervisor when needed and answers
                // `attach_direct` with the `state_dir` the frontend attaches
                // to (L1b, the U3 client). An unknown target has nothing
                // to attach to.
                let Some(ws) = workspaces.workspace_for_tmux(requested_target) else {
                    let payload = serde_json::json!({
                        "error": format!("no workspace owns session {requested_target:?}"),
                        "code": "no_workspace",
                    });
                    write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                        .await?;
                    continue;
                };
                let state_root = sot_log::state_dir::sot_state_dir();
                // `attach_direct` answers at once from memory, no
                // lane probe here -- `ensure_started` runs
                // fire-and-forget in the background under its own
                // guard, so a stale cached `Ready` never blocks it.
                #[cfg(any(windows, target_os = "linux"))]
                {
                    match state_root.clone() {
                        None => {
                            ws.set_activation_error(Some(format!(
                                "could not resolve this machine's state root ({} unset)",
                                crate::capsule_workspace::STATE_ROOT_HINT
                            )));
                        }
                        Some(root) => {
                            let workspace_id = ws.workspace_id.clone();
                            let workspace_id_for_log = workspace_id.clone();
                            let agent_kind = ws.agent();
                            let agent_name = ws.agent_name();
                            let slug = ws.slug.clone();
                            let project_root = ws.project_root.clone();
                            let workspaces_for_start = workspaces.clone();
                            tokio::spawn(async move {
                                wait_for_test_activation_barrier().await;
                                let result = tokio::task::spawn_blocking(move || {
                                    crate::capsule_workspace::ensure_started(
                                        &root,
                                        &workspace_id,
                                        &agent_kind,
                                        &agent_name,
                                        &slug,
                                        &project_root,
                                        crate::capsule_workspace::ActivationIntent::Selection,
                                        workspaces_for_start,
                                    )
                                })
                                .await
                                .unwrap_or_else(|e| {
                                    Err(format!("capsule start-on-attach task panicked: {e}"))
                                });
                                match result {
                                    Ok(Some(())) => {
                                        tracing::info!(workspace_id = %workspace_id_for_log, "pty.open: capsule supervisor started on attach");
                                    }
                                    Ok(None) => {}
                                    Err(detail) => {
                                        tracing::warn!(workspace_id = %workspace_id_for_log, error = %detail, "pty.open: capsule supervisor start-on-attach failed");
                                    }
                                }
                                record_test_activation_marker("completions");
                            });
                        }
                    }
                }
                let state_dir = state_root
                    .map(|root| crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id))
                    .map(|p| p.to_string_lossy().into_owned());
                let payload = serde_json::json!({
                    "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
                    "code": "attach_direct",
                    "state_dir": state_dir,
                });
                write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                    .await?;
                continue;
            }
            op::PTY_INPUT => {
                // ADR 0042 amendment (2026-09-07): answered, unlike
                // `PTY_WRITE` above (this connection's own pty, fire-and-
                // forget) — `PtyInputReq` is untouched by that arm, and
                // vice versa. `origin`, when present, is the handler's own
                // job to validate/prefer; this connection's `hello`
                // `client_id` is only the FALLBACK controller id.
                let default_controller_id = client_guard
                    .as_ref()
                    .map(|g| g.client_id().to_string())
                    .unwrap_or_default();
                handlers::handle_pty_input(frame.id, frame.payload, &workspaces, &default_controller_id)
                    .await
            }
            op::PTY_SCREEN => {
                // ADR 0042 amendment (2026-09-07): a watcher-only read —
                // no controller id needed, it never takes the pen.
                handlers::handle_pty_screen(frame.id, frame.payload, &workspaces).await
            }
            op::MONITOR_SUBSCRIBE => {
                // Open this connection's live tick delivery (sampling is
                // already running). Reply with the host roster + base cadence
                // so the frontend can lay out panels before the first tick.
                monitor_subscribed = true;
                let hosts = workspaces
                    .monitor_hub()
                    .map(|h| h.host_names())
                    .unwrap_or_default();
                let res = MonitorSubscribeRes {
                    interval_s: 1.0,
                    hosts,
                };
                Ok(vec![(
                    Frame::res(frame.id, op::MONITOR_SUBSCRIBE, serde_json::to_value(res)?),
                    None,
                )])
            }
            op::MONITOR_UNSUBSCRIBE => {
                monitor_subscribed = false;
                Ok(vec![(
                    Frame::res(frame.id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
                    None,
                )])
            }
            op::MONITOR_HISTORY => serde_json::from_value::<MonitorHistoryReq>(frame.payload)
                .context("monitor.history payload")
                .and_then(|req| {
                    let hosts = workspaces
                        .monitor_hub()
                        .map(|h| h.history(&req))
                        .unwrap_or_default();
                    let res = MonitorHistoryRes { hosts };
                    Ok(vec![(
                        Frame::res(frame.id, op::MONITOR_HISTORY, serde_json::to_value(res)?),
                        None,
                    )])
                }),
            other => {
                tracing::warn!(op = %other, transport, "unknown op");
                let payload = serde_json::json!({ "error": format!("unknown op: {other}") });
                Ok(vec![(Frame::res(frame.id, other, payload), None)])
            }
        };

        // Service-time logging + per-request error containment (turns a
        // handler `Err` into one `handler_error` frame instead of ending the
        // connection) — shared with every off-loop job via `finish_dispatch`.
        let out_frames = finish_dispatch(&frame.op, frame.id, transport, dispatch_started, dispatched);

        for (out_frame, out_blob) in out_frames {
            write_reply(&mut tx, out_frame, out_blob).await?;
        }
    }
}

/// Awaits the next file-change from the watcher subscription. When the
/// connection has no watcher (Watcher::spawn failed at startup) this future
/// stays pending forever, leaving the `tokio::select!` arm inactive.
async fn recv_watcher(
    rx: &mut Option<broadcast::Receiver<PreviewChanged>>,
) -> Result<PreviewChanged, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Whether a `preview.changed` event should reach a connection whose active
/// workspace is `active_workspace_id` — the CANONICAL id from this
/// connection's last `workspace.activate` (see `active_workspace`'s
/// declaration in `handle_connection`). Resolves it FRESH against
/// `workspaces` on every call rather than trusting a cached slug/root: a
/// same-slug reinsertion, an uncanonical stored root, or a stale slug match
/// after a destroy-then-recreate can otherwise leak a wrong answer.
///
/// - `None` = never activated (a fresh connection, before its first
///   `workspace.activate`) — keeps seeing every event, exactly as before
///   this filter existed.
/// - `Some(id)` that no longer resolves (the workspace was destroyed since
///   activation) drops EVERY event — deliberately not "send everything":
///   the connection told us it was viewing a specific workspace, and that
///   workspace is gone, so there is no view left to serve traffic to.
/// - `Some(id)` that resolves: the SAME two-path predicate
///   `resolve_preview_changed` applies frontend-side
///   (`rust/frontend/src/gpu.rs`), just evaluated once at fan-out instead of
///   once per received-and-discarded frame — (a) the event is tagged with
///   the active workspace's slug, or (b) workspaces overlap (umbrella roots
///   registered over the same tree, watch budgets capping a watcher's
///   coverage) so the event's absolute path lies under the active
///   workspace's CANONICAL root (`FilesMode::root_path`, not the raw stored
///   `project_root`) even when tagged with a different slug. A tag-only
///   filter would break case (b) — that's why the frontend never used one,
///   and why this mirrors its rule instead of inventing a simpler one.
///
/// Containment reuses `paths::path_within_root` (component-aware; correctly
/// rejects the lookalike sibling `/a/wsx` against root `/a/ws`, and — unlike
/// a bare `/`-only prefix check — handles a native Windows event path from
/// `notify`, which is `\`-separated). `path_within_root` treats the root
/// itself as contained; a `preview.changed` for the root path itself (rare —
/// a rename/touch of the project directory node) is intentionally excluded
/// here, matching this filter's original behaviour.
fn preview_changed_visible(
    change: &PreviewChanged,
    active_workspace_id: Option<&str>,
    workspaces: &Workspaces,
) -> bool {
    let Some(id) = active_workspace_id else {
        return true;
    };
    let Some(ws) = workspaces.resolve(Some(id)) else {
        return false;
    };
    if change.workspace_id.as_deref() == Some(ws.slug.as_str()) {
        return true;
    }
    // `files_mode()` errors only if the root has vanished from disk since
    // construction (or on the very first call, if it never canonicalized at
    // all) — fall back to the tag-only check above rather than guessing at
    // containment with an un-canonicalized path.
    match ws.files_mode() {
        Ok(fm) => {
            let root = fm.root_path();
            change.path != root && paths::path_within_root(&change.path, root)
        }
        Err(_) => false,
    }
}

/// Translates one watcher event into a `preview.changed` evt frame on the
/// wire — but only when `active_workspace_id` says this connection can use
/// it (`preview_changed_visible`); otherwise the event is silently dropped
/// here, before a frame decode/JSON-parse/redraw is spent on the frontend
/// for traffic it would have discarded anyway (the measured flood this
/// filter exists for). Returns `Ok(true)` if a frame was written, `Ok(false)`
/// if the receiver was lagged/closed, or if the event was filtered for this
/// connection's active workspace (skip and keep the connection alive either
/// way).
///
/// No `.with_rev(...)` on the outgoing frame, deliberately: `preview.changed`
/// is NOT part of the session revision ring (`watcher.rs` never calls
/// `Session::bump` for it) — see that module's header comment for why a
/// reconnect doesn't need to replay these. Stamping a revision here would
/// silently re-couple this event to the ring's watermark bookkeeping the
/// other half of that fix removes.
async fn write_preview_changed<W>(
    tx: &mut W,
    change: Result<PreviewChanged, broadcast::error::RecvError>,
    transport: &'static str,
    active_workspace_id: Option<&str>,
    workspaces: &Workspaces,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            if !preview_changed_visible(&c, active_workspace_id, workspaces) {
                return Ok(false);
            }
            let payload = serde_json::json!({
                "path": c.path.to_string_lossy(),
                "node_id": c.node_id,
                "kind": c.kind.as_str(),
                "workspace_id": c.workspace_id,
            });
            let frame = Frame::evt(op::PREVIEW_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "preview watcher lagged on this connection; client missed file events"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "preview watcher channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next workspace lifecycle event. The channel is always present
/// (created unconditionally in `run`), so unlike `recv_watcher` this takes a
/// plain receiver rather than an `Option`.
async fn recv_ws_events(
    rx: &mut broadcast::Receiver<WorkspaceChanged>,
) -> Result<WorkspaceChanged, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one workspace lifecycle event into a `workspace.changed` evt
/// frame on the wire. Returns `Ok(true)` if a frame was written, `Ok(false)`
/// if the receiver was lagged or closed (skip and keep the connection alive).
async fn write_workspace_changed<W>(
    tx: &mut W,
    change: Result<WorkspaceChanged, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({
                "action": c.action,
                "slug": c.slug,
                "workspace_id": c.workspace_id,
            });
            let frame = Frame::evt(op::WORKSPACE_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "workspace event bus lagged on this connection; client missed workspace changes"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "workspace event bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next topology write (plan §B). Same shape as `recv_ws_events`
/// — the channel is always present (created unconditionally in `run`).
async fn recv_topo_changed(
    rx: &mut broadcast::Receiver<crate::topology_store::TopologyChanged>,
) -> Result<crate::topology_store::TopologyChanged, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one topology write into a `topology.changed` evt frame.
/// Mirrors `write_workspace_changed`.
async fn write_topology_changed<W>(
    tx: &mut W,
    change: Result<crate::topology_store::TopologyChanged, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({ "hash": c.hash });
            let frame = Frame::evt(op::TOPOLOGY_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "topology event bus lagged on this connection; client missed a topology change"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "topology event bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next relayed agent message. The channel is always present
/// (created unconditionally in `run`), so like `recv_ws_events` this takes a
/// plain receiver rather than an `Option`.
async fn recv_agent_msg(
    rx: &mut broadcast::Receiver<AgentMessage>,
) -> Result<AgentMessage, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one relayed agent message into an `agent.message` evt frame on
/// the wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive).
async fn write_agent_message<W>(
    tx: &mut W,
    msg: Result<AgentMessage, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "from": m.from,
                "to": m.to,
                "text": m.text,
                "ts": m.ts,
            });
            let frame = Frame::evt(op::AGENT_MESSAGE, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "agent relay bus lagged on this connection; client missed messages"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "agent relay bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next FE command (ADR 0025). The channel is always present
/// (created unconditionally in `run`), so like `recv_agent_msg` this takes a
/// plain receiver rather than an `Option`.
async fn recv_fe_command(
    rx: &mut broadcast::Receiver<FeCommandEvt>,
) -> Result<FeCommandEvt, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one FE command into an `fe.command` evt frame on the wire
/// (ADR 0025). Returns `Ok(true)` if a frame was written OR correctly
/// filtered out for this connection, `Ok(false)` if the receiver was lagged
/// or closed (skip and keep the connection alive). The evt is broadcast to
/// every connection; ordinarily the FE self-filters on `target`, but when
/// `e.target_serial` names a specific connection (2026-09-08 review rework,
/// design point B — exclusive delivery by connection identity, not merely a
/// handle string that two connections could share) this connection drops
/// the event outright, without writing anything, unless its own `my_serial`
/// matches. An explicit `--fe <handle>` send carries `target_serial: None`
/// and keeps today's behaviour: every connection gets it and self-filters
/// on `target`.
async fn write_fe_command<W>(
    tx: &mut W,
    evt: Result<FeCommandEvt, broadcast::error::RecvError>,
    transport: &'static str,
    my_serial: Option<u64>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match evt {
        Ok(e) => {
            if let Some(want) = e.target_serial {
                if my_serial != Some(want) {
                    return Ok(true);
                }
            }
            let frame = Frame::evt(op::FE_COMMAND, serde_json::to_value(e)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "fe command bus lagged on this connection; client missed commands"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "fe command bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next streamed REPL frame. The channel is always present (created
/// unconditionally in `run`), so like `recv_agent_msg` this takes a plain
/// receiver rather than an `Option`.
async fn recv_repl_frame(
    rx: &mut broadcast::Receiver<ReplFrameMsg>,
) -> Result<ReplFrameMsg, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one streamed REPL frame into a `repl.frame` evt frame on the
/// wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive). The
/// `frame` value is passed through verbatim — its `{kind, ...}` shape is
/// kernel-defined, so the backend stays oblivious to new frame kinds.
async fn write_repl_frame<W>(
    tx: &mut W,
    msg: Result<ReplFrameMsg, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "eval_id": m.eval_id,
                "workspace_id": m.workspace_id,
                "frame": m.frame,
            });
            let frame = Frame::evt(op::REPL_FRAME, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "repl frame bus lagged on this connection; client missed frames"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "repl frame bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next monitor tick. Mirrors `recv_watcher`: when the hub wasn't
/// installed the receiver is `None` and this stays pending, leaving the
/// select! arm inactive.
async fn recv_monitor(
    rx: &mut Option<broadcast::Receiver<HostLatest>>,
) -> Result<HostLatest, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Translates one monitor tick into a `monitor.tick` evt (one host per evt; the
/// frontend merges by host). Skips on lag/close, keeping the connection alive.
async fn write_monitor_tick<W>(
    tx: &mut W,
    msg: Result<HostLatest, broadcast::error::RecvError>,
    transport: &'static str,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let evt = MonitorTickEvt { hosts: vec![m] };
            let frame = Frame::evt(op::MONITOR_TICK, serde_json::to_value(evt)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                transport,
                "monitor bus lagged on this connection"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!(transport, "monitor bus channel closed");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn write_frame_within_times_out_on_stuck_peer() {
        // The reaper's core: a peer whose socket buffer is full because it
        // stopped draining (the half-open / Recv-Q stall we hit) must not park
        // the connection task forever — the bounded write trips and errors so
        // `handle_connection` drops the connection.
        use super::write_frame_within;
        use sot_protocol::Frame;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::AsyncWrite;

        struct StuckWriter;
        impl AsyncWrite for StuckWriter {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Pending
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
        }

        let frame = Frame::evt("test.stall", serde_json::json!({"k": "v"}));
        let res = write_frame_within(
            &mut StuckWriter,
            &frame,
            None,
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(
            res.is_err(),
            "a non-draining peer must trip the write timeout"
        );
        assert!(res.unwrap_err().to_string().contains("not draining"));
    }

    // Guards the containment path END TO END, not just at the codec. The write
    // loop in `handle_connection` keeps the connection alive for an over-cap
    // envelope only because it can `downcast_ref::<EnvelopeTooLarge>()` on what
    // `write_frame_within` hands back. The codec-level test proves the codec
    // produces the type; this one proves the type still SURVIVES the server's
    // own wrapper. Without it, a rewrite of that wrapper could silently send
    // every oversize response back to dropping the connection while the whole
    // suite stayed green.
    //
    // Note which rewrites are actually dangerous: adding `.context(...)` is
    // SAFE — anyhow searches the whole chain, and this test pins that so nobody
    // "fixes" a non-problem. What breaks the downcast is reformatting the error
    // into a fresh string (`anyhow!("write failed: {e}")`, `bail!`, or a
    // `map_err` that stringifies), which discards the concrete type.
    #[tokio::test]
    async fn oversize_envelope_stays_downcastable_through_write_frame_within() {
        use super::write_frame_within;
        use anyhow::Context as _;
        use sot_protocol::codec::{EnvelopeTooLarge, MAX_ENVELOPE_BYTES};
        use sot_protocol::Frame;

        let mut sink: Vec<u8> = Vec::new();
        let huge = "x".repeat(MAX_ENVELOPE_BYTES + 1);
        let frame = Frame::res(1, "quarto.open", serde_json::json!({ "html": huge }));
        let err = write_frame_within(
            &mut sink,
            &frame,
            None,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect_err("an over-cap envelope must error");

        assert!(
            err.downcast_ref::<EnvelopeTooLarge>().is_some(),
            "handle_connection's containment matches on this type — if the wrapper \
             stops preserving it, oversize responses silently drop connections again"
        );
        assert!(
            sink.is_empty(),
            "nothing may reach the wire, or containment is unsafe"
        );

        // A `.context()` layer must NOT defeat the match (anyhow walks the chain).
        let wrapped = Err::<(), _>(err).context("write envelope").unwrap_err();
        assert!(
            wrapped.downcast_ref::<EnvelopeTooLarge>().is_some(),
            "context-wrapping is safe; only stringifying the error breaks the downcast"
        );
    }

    // The timeout bail must NOT be mistaken for an over-cap envelope: a peer
    // that stopped draining is a genuinely broken socket and has to stay fatal,
    // so containment must not swallow it.
    #[tokio::test]
    async fn write_timeout_is_not_confused_with_an_oversize_envelope() {
        use super::write_frame_within;
        use sot_protocol::codec::EnvelopeTooLarge;
        use sot_protocol::Frame;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::AsyncWrite;

        struct StuckWriter;
        impl AsyncWrite for StuckWriter {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Pending
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
        }

        let frame = Frame::evt("test.stall", serde_json::json!({"k": "v"}));
        let err = write_frame_within(
            &mut StuckWriter,
            &frame,
            None,
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("a non-draining peer must trip the write timeout");
        assert!(
            err.downcast_ref::<EnvelopeTooLarge>().is_none(),
            "a stuck peer must stay fatal — containment must not catch it"
        );
    }

    #[tokio::test]
    async fn write_frame_within_succeeds_on_healthy_peer() {
        // The complement: a sink that drains instantly never trips the timeout,
        // so the reaper can't false-drop a healthy connection.
        use super::write_frame_within;
        use sot_protocol::Frame;

        let mut sink: Vec<u8> = Vec::new();
        let frame = Frame::evt("test.ok", serde_json::json!({"k": "v"}));
        let res =
            write_frame_within(&mut sink, &frame, None, std::time::Duration::from_secs(5)).await;
        assert!(res.is_ok(), "a healthy peer must not trip the timeout");
        assert!(!sink.is_empty(), "frame bytes should have been written");
    }

    #[test]
    fn write_deadline_scales_with_blob_size() {
        // Regression for the false-drop of a legit large preview blob: the
        // deadline must be the tight floor for small/no-blob frames (reaper stays
        // sharp) and grow proportionally for a bulk blob so a draining 71 MB
        // render isn't reaped mid-write.
        use super::{write_deadline, MIN_BLOB_DRAIN_RATE, WRITE_TIMEOUT};
        assert_eq!(write_deadline(None), WRITE_TIMEOUT, "no blob → floor");
        let small = vec![0u8; MIN_BLOB_DRAIN_RATE as usize - 1];
        assert_eq!(
            write_deadline(Some(&small)),
            WRITE_TIMEOUT,
            "sub-rate blob → floor (no grace yet)"
        );
        let big = vec![0u8; 5 * MIN_BLOB_DRAIN_RATE as usize];
        assert_eq!(
            write_deadline(Some(&big)),
            WRITE_TIMEOUT + std::time::Duration::from_secs(5),
            "deadline = floor + size / MIN_BLOB_DRAIN_RATE"
        );
    }


    // Per-connection preview.changed fan-out filter (the flood fix): a
    // connection must see exactly what its ACTIVATED workspace can use —
    // same workspace tag, or a foreign-tagged event whose path is under the
    // active root (workspaces overlap by design) — and nothing else, except
    // before any `workspace.activate` has landed at all (send everything)
    // and after an activated id stops resolving (send nothing until the
    // next activate). Real on-disk roots throughout: `preview_changed_visible`
    // resolves through `Workspace::files_mode()`, which canonicalizes
    // (paths.rs `simplify_verbatim`) — exactly the "uncanonical stored root"
    // failure mode this rework closes, so a literal `PathBuf` root would
    // test nothing.
    mod preview_changed_fanout {
        use super::super::preview_changed_visible;
        use crate::watcher::{ChangeKind, PreviewChanged};
        use crate::workspaces::{Workspace, Workspaces};
        use std::path::PathBuf;

        fn change(workspace_id: Option<&str>, path: PathBuf) -> PreviewChanged {
            PreviewChanged {
                path,
                node_id: Some("files:x".to_string()),
                kind: ChangeKind::Modified,
                workspace_id: workspace_id.map(str::to_string),
            }
        }

        /// A registry with one real, on-disk workspace named `slug`.
        /// Returns (registry, its CANONICAL workspace_id, its canonicalized
        /// root). Caller removes the returned root's directory when done.
        fn registry_with_workspace(tag: &str, slug: &str) -> (Workspaces, String, PathBuf) {
            let dir = std::env::temp_dir().join(format!(
                "sot-preview-fanout-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let workspaces = Workspaces::new();
            let ws = workspaces.insert(Workspace::from_label(
                slug,
                dir,
                false,
                "none".to_string(),
                String::new(),
                String::new(),
            ));
            let root = ws.files_mode().unwrap().root_path().to_path_buf();
            (workspaces, ws.workspace_id.clone(), root)
        }

        #[test]
        fn same_workspace_tag_is_sent() {
            let (workspaces, id, root) = registry_with_workspace("tag", "alpha");
            // Trusted on the tag alone — the path doesn't even need to be
            // real or nearby.
            let c = change(Some("alpha"), PathBuf::from("/anywhere/at/all"));
            assert!(preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn foreign_tag_under_active_root_is_sent() {
            // The umbrella-workspace / watch-budget case: tagged "beta" but
            // the path is really inside the active workspace's own
            // (canonical, on-disk) tree.
            let (workspaces, id, root) = registry_with_workspace("under", "alpha");
            let c = change(Some("beta"), root.join("src").join("main.jl"));
            assert!(preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn foreign_tag_elsewhere_is_dropped() {
            let (workspaces, id, root) = registry_with_workspace("elsewhere", "alpha");
            let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
            assert!(!preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn no_active_workspace_yet_sends_everything() {
            // Fresh connection, before its first `workspace.activate` —
            // today's (pre-filter) behaviour. Registry is irrelevant: `None`
            // short-circuits before it's ever consulted.
            let workspaces = Workspaces::new();
            let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
            assert!(preview_changed_visible(&c, None, &workspaces));
        }

        #[test]
        fn activated_id_no_longer_registered_drops_everything() {
            // The workspace was destroyed since this connection activated
            // it (or the activate itself named something that never
            // resolved) — drop, don't fall back to "send everything": the
            // registry has nothing to fall back TO.
            let workspaces = Workspaces::new();
            let c = change(Some("alpha"), PathBuf::from("/repos/alpha/src/main.jl"));
            assert!(!preview_changed_visible(
                &c,
                Some("ws-no-longer-exists"),
                &workspaces
            ));
        }
    }

    mod project_comm_registry_tests {
        // LU5d2: the registry-watch task's own change detection must see a
        // `host` edit as a real change (a stale row changing owner is not a
        // no-op) even though every other field stayed the same.
        use super::super::project_comm_registry;

        fn registry(host: &str) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "agents": {
                    "host-4-be-x": {
                        "state": "idle",
                        "summary": "",
                        "status_at": "",
                        "tmux": "sot-be-x:0.0",
                        "host": host
                    }
                }
            }))
            .unwrap()
        }

        #[test]
        fn a_host_change_alone_changes_the_projection() {
            let before = project_comm_registry(&registry("hostA"));
            let after = project_comm_registry(&registry("hostB"));
            assert_ne!(before, after);
        }

        #[test]
        fn an_unchanged_registry_projects_identically() {
            let a = project_comm_registry(&registry("hostA"));
            let b = project_comm_registry(&registry("hostA"));
            assert_eq!(a, b);
        }
    }
}
