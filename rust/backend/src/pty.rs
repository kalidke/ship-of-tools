// pty.rs — per-connection pty hosting the shared LLM-pane tmux.
//
// Each connection owns at most one Pty. On `pty.open` the backend
// spawns `tmux new-session -A -s sot-llm` on a `portable-pty`
// PtyPair, sized to the dimensions the frontend asks for. `-A` means
// "attach if it exists, otherwise create" so every frontend launch
// reattaches to the same session — scrollback, running processes,
// claude-code state all persist across launches.
//
// The reader half of the pty is pumped on a dedicated tokio task that
// forwards bytes to an mpsc channel; the server's main connection
// loop drains the channel and emits `pty.evt` frames. Writes go
// directly into the master end via `pty.write`.
//
// Auto-respawn: when the reader sees EOF (`Ok(0)` — child exited /
// tmux server died), the same thread spawns a fresh tmux pair sized
// to the most recent `pty.resize` and swaps it into the shared
// master/writer/child slots, then keeps reading. The frontend
// observes nothing more than a brief data pause (a few hundred ms,
// however long `tmux new-session -A` takes); no protocol envelope
// changes. Per the 17:27Z bus note option 1.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task;

/// Bytes pumped out of the pty. The server loop forwards each chunk
/// to the frontend as a `pty.evt` frame (base64-encoded).
pub type PtyByteReceiver = UnboundedReceiver<Vec<u8>>;

pub struct Pty {
    /// Owns the master end so we can resize. `MasterPty` is
    /// `Send + !Sync`; behind a `Mutex` so the server task can
    /// resize on demand without fighting the writer task. Swapped
    /// in place by the reader thread on auto-respawn.
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Cached writer to the slave end. portable-pty's `take_writer`
    /// is meant to be called once per pty; we own that single
    /// writer here and use it for every `write()`. Swapped in place
    /// by the reader thread on auto-respawn.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Tmux session name this pty is attached to. Defaults to
    /// `sot-llm`; Sessions mode (ADR 0013) passes a per-backend
    /// session name to re-target. Shared with the reader thread so its
    /// auto-respawn path re-opens against the same target.
    target: Arc<Mutex<String>>,
    /// Last (cols, rows) the frontend asked for. Persisted across
    /// respawns so the new tmux child opens at the correct size
    /// instead of reverting to the placeholder. Updated by `resize`.
    size: Arc<Mutex<(u16, u16)>>,
    /// Per-resize timing window. Lets the server loop attribute pty.evt
    /// byte bursts to the resize that triggered them, so we can
    /// distinguish "slow handler" (master.resize is itself slow) from
    /// "slow tmux start" (first byte after resize takes a while) from
    /// "big repaint" (lots of bytes flow back). The window opens on
    /// each resize and the spawned summary task closes it ~2s later.
    /// See 18:17Z bus ask.
    watch: Arc<Mutex<ResizeWatch>>,
    /// Set by `shutdown` (re-target) / `Drop` to tell the reader thread the
    /// EOF it is about to see is a *deliberate* teardown, not a crash — so it
    /// breaks instead of running the auto-respawn / resurrection-guard path.
    /// Without this, detaching the client to release the session cleanly would
    /// itself trip the EOF respawn and re-attach a fresh client. Shared with
    /// the reader thread.
    stopping: Arc<AtomicBool>,
    /// Receiver end of the pty reader pump. The server task `recv`s
    /// from this and emits `pty.evt` frames.
    pub rx: PtyByteReceiver,
}

/// Resize-window state. All fields cleared at the close of each window
/// by the summary task. `summary_task_running` keeps us from spawning
/// overlapping summary tasks if resizes arrive in quick succession —
/// the latest resize just refreshes the window and the existing task
/// is the one that will close it.
#[derive(Default)]
struct ResizeWatch {
    last_resize_at: Option<Instant>,
    last_resize_dims: Option<(u16, u16)>,
    /// Microseconds spent inside `master.resize` on the last resize.
    /// Surfaces in the burst summary so we can tell if the syscall
    /// itself was slow vs the downstream repaint.
    last_handler_us: u128,
    /// Bytes forwarded out of the pty during this window.
    bytes_in_window: u64,
    /// First byte after resize — first_byte_at - last_resize_at tells
    /// how long tmux took to start producing output.
    first_byte_at: Option<Instant>,
    /// Set while a summary task is sleeping. Prevents overlapping
    /// summaries; the most recent resize refreshes the window in place.
    summary_task_running: bool,
}

/// How long the summary task waits before logging. 2 seconds is long
/// enough to capture even a slow tmux repaint over SSH but short
/// enough that the log stays close to the user-observable event.
const RESIZE_BURST_WINDOW: Duration = Duration::from_millis(2000);

/// Default tmux session for the BL pane when the frontend doesn't ask
/// for anything specific. Matches the historical hardcoded name from
/// phase 1 — Sessions mode (ADR 0013) overrides via `target`.
pub const DEFAULT_TMUX_TARGET: &str = "sot-llm";

impl Pty {
    /// Spawn `tmux new-session -A -s <target>` on a pty sized
    /// (cols, rows). The session is shared across launches — first
    /// caller creates it, every subsequent caller reattaches.
    /// `target = None` uses `DEFAULT_TMUX_TARGET`.
    ///
    /// `cwd` is the project root the session is created in (`-c <cwd>`),
    /// so the orchestrator's shell roots at the workspace rather than the
    /// daemon's launch dir. `None` omits `-c` (home-base default). Only
    /// honoured when this call actually *creates* the session — `-A`
    /// ignores `-c` when attaching to an existing one. The value is
    /// remembered by the reader thread so an auto-respawn recreates the
    /// session in the same root.
    ///
    /// `slug` is the owning workspace's slug (`sot-be-<slug>` → `slug`),
    /// stamped into the session env as `SOT_WORKSPACE`. `None` for the
    /// home-base default.
    pub fn spawn(
        cols: u16,
        rows: u16,
        target: Option<&str>,
        cwd: Option<&Path>,
        slug: Option<&str>,
    ) -> Result<Self> {
        let target_name = target.unwrap_or(DEFAULT_TMUX_TARGET).to_string();
        let TmuxPair { master, writer, child, reader } =
            spawn_tmux_pair(cols, rows, &target_name, cwd, slug)?;
        let master = Arc::new(Mutex::new(master));
        let writer = Arc::new(Mutex::new(writer));
        // The child is NOT shared: the reader thread is its sole owner (spawn,
        // exit-observation, reap, respawn all happen there). Sharing it behind
        // Arc<Mutex> only ever produced the zombie leak — the old swap dropped
        // the Box without wait()ing it. Moved in below.
        let target = Arc::new(Mutex::new(target_name));
        let size = Arc::new(Mutex::new((cols.max(1), rows.max(1))));
        let watch = Arc::new(Mutex::new(ResizeWatch::default()));
        let stopping = Arc::new(AtomicBool::new(false));

        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let master_for_reader = Arc::clone(&master);
        let writer_for_reader = Arc::clone(&writer);
        let target_for_reader = Arc::clone(&target);
        let size_for_reader = Arc::clone(&size);
        let stopping_for_reader = Arc::clone(&stopping);
        // Owned by the reader thread alone (not shared back to `Pty`):
        // unlike target/size it's never updated from the server side, so
        // a plain `Option<PathBuf>` the loop mutates locally on the
        // home-base fallback is all it needs.
        let cwd_for_reader = cwd.map(Path::to_path_buf);
        // Same lifecycle as cwd_for_reader: owned by the reader thread and
        // cleared on the home-base fallback, so a respawn there carries no
        // stale workspace identity.
        let slug_for_reader = slug.map(|s| s.to_string());
        // Blocking reader on a dedicated thread — portable-pty's
        // master reader is sync `Read`, not async, so we use
        // `task::spawn_blocking` to keep it off the runtime. The
        // thread also owns the auto-respawn path: on EOF it spawns
        // a fresh tmux pair sized to the latest known dims, swaps
        // it into the shared slots, and keeps reading.
        task::spawn_blocking(move || {
            run_reader_loop(
                reader,
                tx,
                child,
                master_for_reader,
                writer_for_reader,
                target_for_reader,
                size_for_reader,
                stopping_for_reader,
                cwd_for_reader,
                slug_for_reader,
            );
        });

        Ok(Self {
            master,
            writer,
            target,
            size,
            watch,
            stopping,
            rx,
        })
    }

    /// Release this pty's tmux session cleanly so a re-target (or connection
    /// drop) leaves the session alive for a clean re-attach. Used by the
    /// Sessions-mode re-target path before it drops the `Pty` and spawns a
    /// fresh one against the new target.
    ///
    /// Two things have to happen, in order:
    ///   1. Flag the reader thread `stopping` so the EOF it is about to see is
    ///      treated as a deliberate teardown — it must NOT run the auto-respawn
    ///      / resurrection guard (which would re-attach a fresh client to the
    ///      very session we're trying to release, or recreate it).
    ///   2. `tmux detach-client -s <target>` — detach our client from the
    ///      session *without killing it*. This is the fix for the
    ///      left-behind-session-destroyed bug: simply dropping the `Pty` closed
    ///      the master fd abruptly while our client was still attached, and in
    ///      the rapid re-target case that raced tmux's client teardown and took
    ///      the (clientless) session down with it — a bare-shell workspace lost
    ///      its bash on every switch-away. An explicit detach is tmux's
    ///      contract for "client leaves, session stays".
    ///
    /// The detach makes the reader's `read()` return EOF; with `stopping` set it
    /// breaks and releases its master/writer/child clones, so the fd closes with
    /// no client attached and nothing for tmux to SIGHUP.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let target = self.target();
        // Detach any client we have on this session. `-s <session>` targets the
        // session, not a pane; only our pty is attached to these backend
        // sessions, so this affects nothing else. Best-effort: a session that
        // already went away (server death) just errors out harmlessly, and the
        // reader's EOF path handles real recovery when `stopping` is false.
        crate::tmux::TmuxClient::new().detach_session_clients(&target);
    }

    /// Current tmux target name. Sessions-mode re-target compares this
    /// against the requested target so we only kill+respawn when it
    /// actually changes.
    pub fn target(&self) -> String {
        self.target
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| DEFAULT_TMUX_TARGET.to_string())
    }

    /// Write user keystroke bytes to the pty.
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("pty writer mutex poisoned"))?;
        writer.write_all(bytes).context("pty write_all")?;
        writer.flush().context("pty flush")?;
        Ok(())
    }

    /// Resize the pty (and the underlying tty, so the running tmux
    /// sees `SIGWINCH` and reflows). The new dims are also stashed
    /// so a subsequent auto-respawn opens at the right size.
    ///
    /// Times the `master.resize` syscall and (re)opens a 2-second
    /// resize-burst window so the server loop can attribute any
    /// pty.evt bytes that follow back to this resize. A summary task
    /// is spawned to log totals (handler_us, first_byte_ms,
    /// bytes_in_window) when the window closes — see 18:17Z bus ask
    /// re wall-clock cost of Alt+- restore.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Update the cached size first so a respawn racing this call
        // sees the latest dims even if the master.resize() below
        // happens to fail.
        if let Ok(mut s) = self.size.lock() {
            *s = (cols, rows);
        }
        let call_start = Instant::now();
        let master = self
            .master
            .lock()
            .map_err(|_| anyhow!("pty master mutex poisoned"))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("pty resize: {e}"))?;
        let handler_us = call_start.elapsed().as_micros();
        // Drop the master lock before doing the spawn so we don't hold
        // it across the tokio handoff.
        drop(master);

        let need_spawn = {
            let mut w = self
                .watch
                .lock()
                .map_err(|_| anyhow!("pty watch mutex poisoned"))?;
            w.last_resize_at = Some(call_start);
            w.last_resize_dims = Some((cols, rows));
            w.last_handler_us = handler_us;
            w.bytes_in_window = 0;
            w.first_byte_at = None;
            let need = !w.summary_task_running;
            if need {
                w.summary_task_running = true;
            }
            need
        };
        tracing::info!(cols, rows, handler_us, "pty.resize handler done");

        if need_spawn {
            let watch = Arc::clone(&self.watch);
            tokio::spawn(async move {
                tokio::time::sleep(RESIZE_BURST_WINDOW).await;
                if let Ok(mut w) = watch.lock() {
                    if let Some(resize_at) = w.last_resize_at {
                        let dims = w.last_resize_dims.unwrap_or((0, 0));
                        let total_ms = resize_at.elapsed().as_millis();
                        let first_byte_ms = w
                            .first_byte_at
                            .map(|t| t.duration_since(resize_at).as_millis() as i64)
                            .unwrap_or(-1);
                        tracing::info!(
                            cols = dims.0,
                            rows = dims.1,
                            handler_us = w.last_handler_us,
                            first_byte_ms,
                            bytes_in_window = w.bytes_in_window,
                            window_ms = total_ms,
                            "pty.resize burst summary"
                        );
                    }
                    *w = ResizeWatch::default();
                }
            });
        }
        Ok(())
    }

    /// Server loop hook: record a chunk of bytes flowing out of the
    /// pty so the resize-burst summary can attribute them to the
    /// triggering resize. No-op when no resize window is open.
    pub fn note_outgoing_bytes(&self, len: usize) {
        let Ok(mut w) = self.watch.lock() else {
            return;
        };
        let Some(resize_at) = w.last_resize_at else {
            return;
        };
        if w.first_byte_at.is_none() {
            w.first_byte_at = Some(Instant::now());
            tracing::info!(
                first_byte_ms = resize_at.elapsed().as_millis(),
                "pty.resize first byte"
            );
        }
        w.bytes_in_window = w.bytes_in_window.saturating_add(len as u64);
    }
}

impl Drop for Pty {
    /// Safety net for the connection-close path (the FE disconnects and the
    /// `Pty` drops without an explicit `shutdown`). Same intent as `shutdown`:
    /// flag the reader as deliberate-teardown and detach our client so the tmux
    /// session is released cleanly instead of being taken down by the abrupt
    /// master-fd close. Idempotent with `shutdown` (the re-target path calls
    /// that first; re-flagging + re-detaching here is harmless).
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What `spawn_tmux_pair` returns: a sized + spawned tmux child plus
/// the master/writer/reader handles to drive it. Wrapping in a struct
/// (rather than a 4-tuple) makes the call sites readable.
struct TmuxPair {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
}

/// Open a fresh pty pair sized (cols, rows) and spawn
/// `tmux new-session -A -s <target>` on it. Used by the initial
/// `Pty::spawn` and by the reader thread's auto-respawn path. The
/// `; set -g mouse on` argv tail is what enables tmux to honour the
/// frontend's SGR wheel-passthrough sequences (xterm mouse-tracking).
///
/// `cwd`, when `Some`, is passed as `-c <cwd>` so a *created* session
/// roots there (the orchestrator's project, not the daemon's launch
/// dir). tmux ignores `-c` when `-A` attaches to an existing session,
/// so a stale session created in the wrong dir must be killed once to
/// pick up the right root — fresh creates are correct from here on.
fn spawn_tmux_pair(
    cols: u16,
    rows: u16,
    target: &str,
    cwd: Option<&Path>,
    slug: Option<&str>,
) -> Result<TmuxPair> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow!("openpty: {e}"))?;

    // Private per-user socket (security review, mirrors `tmux.rs`'s
    // `TmuxClient::run`). Global `-S` MUST precede the subcommand.
    // F2: propagate a failed/insecure dir check (F1's `secure_private_dir`)
    // instead of ignoring it and spawning tmux against an unverified —
    // possibly hijacked — directory.
    let socket = crate::paths::tmux_socket_path();
    if let Some(dir) = socket.parent() {
        crate::paths::secure_private_dir(dir)
            .with_context(|| format!("securing tmux socket dir {}", dir.display()))?;
    }
    // `tmux new-session -A -s <target>`: create the session if
    // it doesn't exist, attach if it does. -A is the relevant
    // flag (vs -d which would refuse to attach).
    let mut cmd = CommandBuilder::new("tmux");
    cmd.arg("-S");
    cmd.arg(&socket);
    cmd.arg("new-session");
    cmd.arg("-A");
    cmd.arg("-s");
    cmd.arg(target);
    // Root a freshly-created session at the workspace project dir. No-op
    // when `-A` attaches to an existing session (tmux ignores `-c` then).
    if let Some(dir) = cwd {
        cmd.arg("-c");
        cmd.arg(dir.as_os_str());
    }
    // Ship of Tools awareness env. tmux's `-e VAR=val` on `new-session` sets the
    // var on the NEW session — a plain child-process env var (cmd.env, below for
    // TERM) does NOT propagate: tmux derives a new session's env from the server
    // plus the connecting client's `update-environment` allowlist, dropping
    // arbitrary vars. `-e` is honoured on create and ignored by `-A` on attach
    // (same as `-c`). A session in the pane reads these to detect it is inside
    // Ship of Tools, which workspace it is in, and to drive the FE.
    //
    // BUT `-e` on `new-session` is a tmux >= 3.2 flag. On 3.0a (Ubuntu 20.04
    // userland — exactly the old lab backends this app targets) the client
    // rejects it at arg-parse and exits in ~4ms; pre-fix that drove an
    // unthrottled reader respawn loop (~150/s) and a 339k-zombie fork bomb
    // (expectations 2026-07-11 report). So probe the tmux version ONCE, gate `-e`
    // on it, and fail CLOSED — an unknown/absent/unparseable version omits `-e`
    // rather than risk the storm.
    let supports_e = tmux_supports_dash_e();
    let env = awareness_env(slug, cwd);
    if supports_e {
        for (k, v) in &env {
            cmd.arg("-e");
            cmd.arg(format!("{k}={v}"));
        }
    }
    // tmux treats a literal `;` argv token as its own command separator and
    // runs `set -g mouse on` against the resulting session. Idempotent; sticks
    // for the lifetime of the tmux server.
    cmd.arg(";");
    cmd.arg("set");
    cmd.arg("-g");
    cmd.arg("mouse");
    cmd.arg("on");
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow!("spawn tmux: {e}"))?;
    // Slave side is owned by the child process now; drop our handle
    // so the pty close notifies the child cleanly when we drop master.
    drop(pair.slave);

    if !supports_e {
        // tmux < 3.2: `-e` was omitted above to dodge the arg-parse storm.
        // Recover the awareness vars best-effort via `set-environment` on a short
        // detached thread (the session needs a beat to come up). NOTE this only
        // reaches FUTURE processes in the session — tmux's session environment
        // is copied into a process's env at spawn time, so the pane's ALREADY
        // running initial shell won't retroactively see them. On old tmux,
        // home-base awareness is therefore a documented best-effort degrade, not
        // a guarantee; every SOT_* consumer has a fallback (sot-nav.sh derives
        // the slug from the `sot-be-<slug>` session name). See docs/INSTALL-AGENT.md.
        best_effort_session_env(target, env);
    }

    let master: Box<dyn MasterPty + Send> = pair.master;
    let reader = master
        .try_clone_reader()
        .map_err(|e| anyhow!("clone pty reader: {e}"))?;
    let writer = master
        .take_writer()
        .map_err(|e| anyhow!("take pty writer: {e}"))?;
    Ok(TmuxPair { master, writer, child, reader })
}

/// The `SOT_*` awareness env for a tmux session the daemon owns: `SOT_SESSION=1`
/// ("you are inside Ship of Tools"), the owning workspace's slug + project root,
/// and the product checkout for the help persona. The one builder shared by
/// every session-creation path (`Pty::spawn`'s `new-session -A`,
/// `TmuxClient::create_session`, and the boot-time repair sweep) so the paths
/// can't drift on WHAT gets stamped.
pub(crate) fn awareness_env(slug: Option<&str>, cwd: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![("SOT_SESSION".to_string(), "1".to_string())];
    if let Some(slug) = slug {
        env.push(("SOT_WORKSPACE".to_string(), slug.to_string()));
    }
    if let Some(dir) = cwd {
        env.push(("SOT_WORKSPACE_ROOT".to_string(), dir.to_string_lossy().into_owned()));
    }
    if let Some(root) = manual_root() {
        env.push(("SOT_MANUAL".to_string(), root.to_string_lossy().into_owned()));
    }
    env
}

/// Clone-based install (ADR 0030 addendum): the pane's agent gets pointed at
/// the product's own checkout — the repo IS the manual, and docs/USING.md is
/// its entry point for a help+extend persona. Resolved through the same chain
/// as every other resource (dev checkouts get the dev tree, installs get
/// $PREFIX/repo/current); `None` when absent. Cached: the answer can't change
/// under a running daemon, and `awareness_env` is called per session spawn +
/// per workspace in the boot sweep (same reasoning as `tmux_supports_dash_e`).
fn manual_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let manual = crate::paths::resource_dir("docs");
        if manual.exists() {
            manual.parent().map(Path::to_path_buf)
        } else {
            None
        }
    })
    .as_deref()
}

/// Does this tmux understand `new-session -e`? The flag arrived in tmux 3.2;
/// older tmux (e.g. 3.0a on Ubuntu 20.04) rejects it at arg-parse and the client
/// dies instantly. Probed ONCE and cached — the answer can't change under a
/// running daemon. Fail-closed: any trouble (absent binary, timeout, a version
/// string we can't parse) returns `false`, so we omit `-e` rather than risk the
/// respawn storm on a tmux we didn't positively confirm supports it.
pub(crate) fn tmux_supports_dash_e() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        match tmux_version() {
            Some((maj, min)) => {
                let ok = (maj, min) >= (3, 2);
                tracing::info!(
                    tmux_major = maj,
                    tmux_minor = min,
                    dash_e = ok,
                    "tmux capability probe: new-session -e {}",
                    if ok { "supported" } else { "UNSUPPORTED (tmux < 3.2) — degrading: omitting -e, awareness env best-effort via set-environment" }
                );
                ok
            }
            None => {
                tracing::warn!(
                    "tmux capability probe: could not determine tmux version (absent / timed out / unparseable) — failing closed, omitting new-session -e. If tmux is missing the backend cannot host the LLM pane; see docs/INSTALL-AGENT.md"
                );
                false
            }
        }
    })
}

/// `(major, minor)` from `tmux -V`, or `None` on any failure. Runs the probe on
/// a throwaway thread with a 3s deadline so a wedged `tmux` binary can't stall
/// the caller (the reader thread / a pty.open). `tmux -V` doesn't touch the
/// server, so it's normally instant.
fn tmux_version() -> Option<(u32, u32)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tmux-version-probe".into())
        .spawn(move || {
            let out = std::process::Command::new("tmux").arg("-V").output();
            let _ = tx.send(out);
        })
        .ok()?;
    let out = rx.recv_timeout(Duration::from_secs(3)).ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_tmux_version(&String::from_utf8_lossy(&out.stdout))
}

/// Parse a `tmux -V` string into `(major, minor)`. Tolerates the real-world
/// shapes: `tmux 3.2`, `tmux 3.0a`, `tmux 3.3a`, `tmux next-3.4`, `tmux 2.9a`.
/// Letter suffixes and a `next-` prefix are stripped; a missing minor reads 0.
fn parse_tmux_version(s: &str) -> Option<(u32, u32)> {
    let v = s.trim().strip_prefix("tmux ")?.trim();
    let v = v.strip_prefix("next-").unwrap_or(v);
    let leading_num = |seg: &str| -> Option<u32> {
        let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    let mut parts = v.split('.');
    let major = leading_num(parts.next()?)?;
    let minor = parts.next().and_then(leading_num).unwrap_or(0);
    Some((major, minor))
}

/// Best-effort injection of the `SOT_*` awareness vars into a tmux session via
/// `set-environment`, used only on tmux < 3.2 where `-e` on `new-session` isn't
/// available. Runs detached with a small delay (the session created by the
/// `new-session` we just spawned needs a beat to exist), and is quiet on
/// failure — it is a best-effort awareness aid for FUTURE processes in the
/// session, never load-bearing (see the note at the call site).
fn best_effort_session_env(target: &str, env: Vec<(String, String)>) {
    let target = target.to_string();
    let _ = std::thread::Builder::new()
        .name("sot-session-env".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            crate::tmux::TmuxClient::new().set_session_env_all(&target, &env);
        });
}

/// Reap a tmux child so it never lingers as a zombie. EOF on the pty almost
/// always means the client already exited, so a non-blocking `try_wait` reaps it
/// immediately; if it is somehow still alive we `kill` and then `wait` (bounded
/// in practice — a killed tmux client exits promptly). Either way we never do an
/// unbounded blocking `wait` on a live child from the reader loop.
fn reap_child(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    match child.try_wait() {
        // Already exited: `try_wait` REAPS here — portable-pty 0.9 delegates to
        // `std::process::Child::try_wait` (= `waitpid(WNOHANG)`, which collects
        // the status). So this arm needs no `wait()`; leaving it empty is correct
        // (verified 2026-07-17, Codex + Fable review). Do NOT rely on this to
        // reap by itself: a `Child` that is merely DROPPED never reaches here —
        // every reader-loop exit and teardown path must call `reap_child`.
        Ok(Some(_status)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(_) => {
            let _ = child.wait();
        }
    }
}

/// Backoff delay for the Nth consecutive short-lived pty failure: 100ms, 250ms,
/// 500ms, 1s, then 2s. Bounds the respawn rate so an instantly-dying client
/// (the tmux < 3.2 case) can't loop ~150×/s.
fn respawn_backoff(consecutive: u32) -> Duration {
    match consecutive {
        0 | 1 => Duration::from_millis(100),
        2 => Duration::from_millis(250),
        3 => Duration::from_millis(500),
        4 => Duration::from_secs(1),
        _ => Duration::from_secs(2),
    }
}

/// A pty child that dies within this window of being spawned is treated as a
/// "short-lived failure" and counted against the breaker; one that lived longer
/// and then died (normal tmux-server death) resets the counter and respawns
/// immediately.
const PTY_HEALTHY_THRESHOLD: Duration = Duration::from_secs(2);

/// After this many consecutive short-lived failures the reader stops the tight
/// respawn loop and enters a longer cooldown before a single controlled retry,
/// rather than either looping hot or giving up forever (a later tmux upgrade
/// should self-heal).
const PTY_GIVE_UP_CAP: u32 = 5;

/// Cooldown after the give-up cap is reached, before one more controlled retry.
const PTY_COOLDOWN: Duration = Duration::from_secs(30);

/// Cap on the recent-pty-output ring we keep so the give-up log can show WHY the
/// client kept dying (on tmux < 3.2 the usage error rides the pty stream).
const PTY_RECENT_CAP: usize = 1024;

/// Does a tmux session with exactly this name exist right now? The `=`
/// prefix forces exact-name match (a bare `-t` prefix-matches, so
/// `has-session -t foo` would say yes for `foo-bar`). A dead or absent
/// tmux server reports "no", which is the right answer for the
/// resurrection-guard either way.
fn session_exists(name: &str) -> bool {
    // Private per-user socket (security review) — same as `spawn_tmux_pair`.
    std::process::Command::new("tmux")
        .arg("-S")
        .arg(crate::paths::tmux_socket_path())
        .args(["has-session", "-t", &format!("={name}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// ADR 0023 §3 — daemon-side claude boot. Open a *throwaway* pty client to the
/// (already-created) `session`, type the `ccb` launcher, poll until claude is the
/// pane's foreground command, then detach. claude survives the detach, so the
/// session ends up running claude with **zero frontend attached** — the
/// no-session-switch background spawn.
///
/// Why a real pty client and not `tmux send-keys`: claude's TUI needs a real
/// terminal at init and exits in a clientless pane (ADR §Context, confirmed
/// empirically). `tmux new-session -A -s <session>` on a portable-pty IS a real
/// client (the session already exists, so `-A` attaches), which satisfies the
/// init requirement; once claude is up, detaching is safe (long-running agents
/// run detached today). The `ccb` script does its own teammate/nesting env scrub
/// (`unset CLAUDECODE …`), so a poisoned tmux/server env can't make this claude
/// mis-detect itself as nested and exit.
///
/// Bounded + best-effort: on a failure to open the boot pty, or if claude is not
/// foreground within `BOOT_TIMEOUT`, it logs `boot_failed` and returns — no
/// infinite retry (mirrors the FE's `AUTOSTART_LAUNCH_HOLD` reasoning). The FE
/// autostart-on-attach remains the fallback for those cases, and the FE
/// foreground guard (ADR §5) prevents a double launch if a user later navigates
/// in. Spawn this as a detached `tokio::spawn` from `handle_workspace_create`;
/// it must NOT block the `workspace.create` response on the multi-second boot.
pub async fn boot_workspace_claude(
    session: String,
    agent_name: String,
    cwd: PathBuf,
    slug: String,
) {
    const BOOT_COLS: u16 = 120;
    const BOOT_ROWS: u16 = 40;
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const BOOT_TIMEOUT: Duration = Duration::from_secs(45);

    // Open a real pty client to the session on a blocking thread (openpty +
    // process spawn). `-A` attaches to the existing session created moments ago
    // by `create_session`; `-c`/`-e` are ignored on attach (tmux semantics), but
    // `create_session` already rooted the session and stamped `awareness_env`.
    let session_spawn = session.clone();
    let cwd_spawn = cwd.clone();
    let slug_spawn = slug.clone();
    let pair = match task::spawn_blocking(move || {
        spawn_tmux_pair(
            BOOT_COLS,
            BOOT_ROWS,
            &session_spawn,
            Some(cwd_spawn.as_path()),
            Some(slug_spawn.as_str()),
        )
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!(%session, error = %e,
                "daemon-boot: failed to open boot pty — boot_failed; FE autostart-on-attach remains the fallback");
            return;
        }
        Err(e) => {
            tracing::warn!(%session, error = %e, "daemon-boot: boot pty spawn task join error");
            return;
        }
    };
    let TmuxPair { master, writer, mut child, reader } = pair;

    // Drain the client's output so its pty buffer never stalls the session. We
    // don't render these bytes — this is a boot client, not a viewer. The thread
    // exits on EOF, which arrives when we detach + drop the master below.
    let drain = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    // No stdin typing: the pane's START COMMAND is the boot wrapper (set at
    // create_session time — see `boot_wrapper_command`), which waits for a client
    // to attach, then `exec`s ccb so claude REPLACES the wrapper as the pane's
    // process. Our `spawn_tmux_pair -A` above is that client (claude needs one at
    // init). This kills the prompt race the old stdin `write_all` had — typing
    // `ccb` before the shell was ready dropped/mangled it and claude never booted
    // (the daemon-boot path failed while a nav-pane create, which attaches a real
    // client, worked). Because ccb is exec'd, `pane_current_command` reads
    // `claude`/`node`, so detection below is unchanged. `writer` is unused now;
    // dropped on detach below.
    tracing::info!(%session, agent = %agent_name,
        "daemon-boot: boot client attached; pane wrapper exec'ing ccb, polling for claude");

    // Poll the pane's foreground command until claude is up or we time out.
    let start = Instant::now();
    let mut booted = false;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let cmd = crate::tmux::TmuxClient::new().active_pane_command(&session);
        if pane_is_claude(cmd.as_deref()) {
            booted = true;
            break;
        }
        if start.elapsed() >= BOOT_TIMEOUT {
            break;
        }
    }

    // Detach our client first (tmux's "client leaves, session stays" contract),
    // THEN drop the master/writer — detaching before the fd close avoids the
    // abrupt-close-while-attached race that can take a clientless session down
    // (the same ordering `Pty::shutdown` relies on). claude (if up) survives.
    crate::tmux::TmuxClient::new().detach_session_clients(&session);
    drop(writer);
    drop(master);
    let _ = drain.join();
    // Reap the detached boot client — a bare `drop(child)` does NOT waitpid, so
    // it leaked one `tmux: client` zombie per workspace create. By here the drain
    // thread has read EOF, so the detached client has exited and `reap_child`'s
    // `try_wait` collects it immediately (no SIGHUP-grace loop).
    reap_child(&mut child);

    if booted {
        tracing::info!(%session,
            "daemon-boot: claude foreground — boot pty detached (claude survives, zero FE attached)");
    } else {
        tracing::warn!(%session,
            "daemon-boot: boot_failed — claude not foreground within timeout; FE autostart-on-attach remains the fallback");
    }
}

/// tmux pane *start command* for a `boot:true` workspace (ADR 0023 §3). tmux runs
/// `[command]` via the shell, so this makes `ccb` the pane's process instead of
/// typing it into a shell prompt — which raced (bytes landing before the shell
/// was ready dropped/mangled the launcher, so the daemon-boot failed while a
/// nav-pane create, which attaches a real client, worked). The wrapper:
///   1. polls the session's attached-client count until a client attaches —
///      `boot_workspace_claude` attaches one, and claude needs a real client at
///      init (the pane inherits `$TMUX`, so a bare `tmux` targets the right
///      server). This also self-heals: a later FE nav-in attach still triggers it.
///   2. `exec`s `ccb` (pinning `SOT_COMM_NAME`), so claude REPLACES the wrapper as
///      the pane's process — `pane_current_command` then reads `claude`/`node` and
///      detection is trivial. `exec` means the pane ends when claude exits (no
///      drop-to-shell), which is fine for a daemon-booted agent session.
/// Mirrors ADR-0017's resume_command (launch-arg, not stdin) to dodge the race.
pub fn boot_wrapper_command(session: &str, agent_name: &str, agent_kind: &str) -> String {
    let env = if agent_name.is_empty() {
        String::new()
    } else {
        format!("export SOT_COMM_NAME={agent_name}; ")
    };
    let mut w = String::new();
    // Bounded wait (FE-flagged gotcha): a client — the boot-pty's `-A` attach —
    // flips session_attached >0 within ~1s; cap the wait at ~30s so a pane whose
    // boot-pty never attaches doesn't hang forever. After the cap it exec's ccb
    // anyway (best-effort; the FE autostart-on-attach path is still the fallback).
    w.push_str("i=0; while [ \"$(tmux display -p -t '");
    w.push_str(session);
    w.push_str("' '#{session_attached}' 2>/dev/null || echo 0)\" = 0 ] && [ \"$i\" -lt 300 ]; do sleep 0.1; i=$((i+1)); done; ");
    // Re-read the tmux session env before exec so the agent inherits the SOT_*
    // vars even when the stamp landed AFTER this shell spawned (the tmux < 3.2
    // `set-environment` fallback, the daemon-boot repair sweep); harmless
    // no-op re-export where `-e` already delivered them. Each var is queried
    // BY EXACT NAME — never a prefix grep over the whole env, which Codex
    // showed is eval-injectable via a hostile var NAME (tmux validates only
    // "no '=' in name") and corrupts multiline values (line filter drops the
    // continuation lines of tmux's quoted serialization). `show-environment
    // -s <name>` emits one eval-able `VAR="val"; export VAR;` command with
    // the value escaped by tmux (verified on 3.0a and next-3.7; unset name →
    // stderr + empty stdout → eval no-op). Keep the name list in lockstep
    // with `awareness_env` (pinned by a test). Session names pass
    // `valid_name` ([A-Za-z0-9._-]), so the single-quoted embedding is safe.
    w.push_str(
        "for _v in SOT_SESSION SOT_WORKSPACE SOT_WORKSPACE_ROOT SOT_MANUAL; do \
         eval \"$(tmux show-environment -s -t '",
    );
    w.push_str(session);
    w.push_str("' \"$_v\" 2>/dev/null)\"; done; ");
    w.push_str(&env);
    // ADR 0031: launcher branch by agent kind. ccx is the codex counterpart
    // of ccb (comm/adapters/codex/bin/ccx); anything unknown falls back to
    // ccb so an old FE against a new daemon can't brick a pane.
    if agent_kind == "codex" {
        w.push_str("exec ~/.local/bin/ccx");
    } else {
        w.push_str("exec ~/.local/bin/ccb");
    }
    w
}

/// Is this pane's foreground command a running claude? Claude Code reports as
/// either `claude` (the launcher) or `node` (its runtime), matching the FE-side
/// `pane_command_is_claude` guard so the daemon-boot and FE-attach paths agree on
/// "claude is up here."
fn pane_is_claude(cmd: Option<&str>) -> bool {
    // `node` covers both Claude Code's runtime and codex-cli's (ADR 0031);
    // `codex` covers a binary-installed codex.
    matches!(cmd, Some("claude") | Some("node") | Some("codex"))
}

/// Sleep up to `dur`, but wake early (returning `true`) if `stopping` gets set —
/// so a deliberate teardown during a backoff/cooldown doesn't wait out the full
/// delay before the reader exits.
fn sleep_unless_stopping(stopping: &AtomicBool, dur: Duration) -> bool {
    let start = Instant::now();
    let step = Duration::from_millis(50).min(dur);
    while start.elapsed() < dur {
        if stopping.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(step);
    }
    stopping.load(Ordering::SeqCst)
}

/// Append pty output to the bounded recent-bytes ring used for the give-up log.
fn append_recent(recent: &mut Vec<u8>, bytes: &[u8]) {
    recent.extend_from_slice(bytes);
    if recent.len() > PTY_RECENT_CAP {
        let overflow = recent.len() - PTY_RECENT_CAP;
        recent.drain(0..overflow);
    }
}

/// Reader-thread loop and pty SUPERVISOR. Owns the tmux child outright — spawn,
/// exit-observation, reap, and respawn all happen here and nothing else holds a
/// reference — drains the master reader into the byte channel, and on child exit
/// respawns a fresh pair sized to the latest cached dims, swapping the new
/// master/writer into the shared slots and taking the new reader.
///
/// Failure breaker (expectations 2026-07-11 report): a child that dies within
/// `PTY_HEALTHY_THRESHOLD` of being spawned is a "short-lived failure" and is
/// counted; consecutive short-lived failures back off (`respawn_backoff`) and,
/// past `PTY_GIVE_UP_CAP`, drop into a `PTY_COOLDOWN` before one more controlled
/// retry — so an instantly-dying client (the tmux < 3.2 arg-parse death, a
/// missing/broken tmux, or any spawn failure) can't loop ~150×/s, yet a later fix
/// (tmux upgrade) still self-heals. A child that lived past the threshold and
/// then died (normal tmux-server death) resets the counter and respawns
/// immediately. The dead child is ALWAYS reaped (`reap_child`) before it is
/// dropped, which closes the zombie leak (the old code swapped a shared
/// `Arc<Mutex<child>>` and dropped the box without `wait()`ing it → 1 zombie per
/// respawn, ~339k of them in the field).
///
/// `stopping`, when set by `Pty::shutdown` / `Drop`, means the EOF/read-error we
/// are about to observe is a deliberate teardown (re-target / connection close) —
/// reap and exit instead of respawning, so the left-behind session is released
/// cleanly without the reader re-attaching to it.
#[allow(clippy::too_many_arguments)]
fn run_reader_loop(
    mut reader: Box<dyn Read + Send>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    target: Arc<Mutex<String>>,
    size: Arc<Mutex<(u16, u16)>>,
    stopping: Arc<AtomicBool>,
    // Project root to recreate the session in (`-c <cwd>`) on respawn. Cleared to
    // `None` on the home-base fallback, since that session is not workspace-rooted.
    mut cwd: Option<PathBuf>,
    // Owning workspace slug (`SOT_WORKSPACE`), same lifecycle as `cwd`.
    mut slug: Option<String>,
) {
    let mut buf = [0u8; 8192];
    // When the current child was spawned — for the short-lived classification.
    let mut spawned_at = Instant::now();
    // Consecutive short-lived failures (reset by a healthy run).
    let mut consecutive: u32 = 0;
    // Last bytes from a YOUNG child, so the give-up log can show why it died (on
    // old tmux the usage error rides the pty stream). Only recorded while the
    // child is young — a healthy long-running pane doesn't pay for this.
    let mut recent: Vec<u8> = Vec::new();

    'reader: loop {
        // The borrow of `reader` ends when `read` returns, so the failure path
        // below is free to reassign `reader` to the respawned one.
        match reader.read(&mut buf) {
            Ok(n) if n > 0 => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    // Receiver dropped ⇒ the `Pty` is being dropped/re-targeted
                    // (`rx` is a `Pty` field). Both drop paths run `shutdown()`
                    // — set `stopping` + `detach-client` — BEFORE `rx` drops, so
                    // this fires ~every switch (the reader loses the race to the
                    // synchronous detach) and the detached client is already
                    // exiting. Reap it here so it doesn't linger as a zombie; a
                    // bare `break` would drop the `Child` un-`wait()`ed. This was
                    // the dominant `tmux: client <defunct>` leak. `reap_child`'s
                    // `kill()` is SIGHUP-then-grace (portable-pty), session-safe.
                    if !stopping.load(Ordering::SeqCst) {
                        tracing::warn!(
                            "pty receiver dropped without shutdown(); reaping client anyway"
                        );
                    }
                    reap_child(&mut child);
                    break;
                }
                if spawned_at.elapsed() < PTY_HEALTHY_THRESHOLD {
                    append_recent(&mut recent, &buf[..n]);
                }
                continue;
            }
            Ok(_) => {
                // EOF — child exited or tmux server died.
                if stopping.load(Ordering::SeqCst) {
                    tracing::info!("pty EOF during shutdown; stopping reader (no respawn)");
                    reap_child(&mut child);
                    break;
                }
            }
            Err(e) => {
                // A read error is unusual; treat it like an EOF failure (reap +
                // breaker + respawn) rather than silently ending the reader.
                if stopping.load(Ordering::SeqCst) {
                    tracing::info!(error = %e, "pty read error during shutdown; stopping reader");
                    reap_child(&mut child);
                    break;
                }
                tracing::warn!(error = %e, "pty read error; treating as child failure and respawning");
            }
        }

        // Shared failure path (EOF or read error, not a deliberate stop).
        // Classify BEFORE reaping (reaping is what confirms the death), then reap
        // so the dead child never lingers as a zombie.
        let short_lived = spawned_at.elapsed() < PTY_HEALTHY_THRESHOLD;
        reap_child(&mut child);
        consecutive = if short_lived { consecutive + 1 } else { 0 };

        // Respawn with breaker pacing, retrying on spawn error. Yields the new
        // (child, reader) or breaks the whole reader loop on a stop request.
        let (new_child, new_reader) = 'respawn: loop {
            if stopping.load(Ordering::SeqCst) {
                break 'reader;
            }
            let (cols, rows) = match size.lock() {
                Ok(g) => *g,
                Err(_) => {
                    tracing::error!("size mutex poisoned; cannot respawn pty");
                    break 'reader;
                }
            };
            let cur_target = match target.lock() {
                Ok(g) => g.clone(),
                Err(_) => {
                    tracing::error!("target mutex poisoned; cannot respawn pty");
                    break 'reader;
                }
            };
            // Never resurrect a dead NAMED session: `new-session -A` against a
            // name that no longer exists CREATES it — bare, cwd=~, no `-c`. When
            // the EOF came from `workspace.destroy` killing the session under an
            // attached client, that resurrection races the follow-up
            // `workspace.create` into "duplicate session", registering a
            // workspace without its owned session (observed live 2026-06-11
            // 20:51Z). Re-attach only if the session still exists; otherwise fall
            // back to the home-base default — recreating THAT is the original
            // recovery semantics (whole tmux-server death), and the target slot is
            // updated so re-target comparisons stay honest.
            let cur_target = if cur_target != DEFAULT_TMUX_TARGET
                && !session_exists(&cur_target)
            {
                tracing::warn!(gone = %cur_target,
                    "pty EOF — target session no longer exists; falling back to home-base instead of resurrecting it");
                if let Ok(mut g) = target.lock() {
                    *g = DEFAULT_TMUX_TARGET.to_string();
                }
                // Home-base is deliberately not workspace-rooted; drop the project
                // cwd (and workspace slug) so the fallback create doesn't pin it to
                // a workspace it no longer represents.
                cwd = None;
                slug = None;
                DEFAULT_TMUX_TARGET.to_string()
            } else {
                cur_target
            };

            // Pace before the (re)spawn: a longer cooldown past the give-up cap,
            // else a short backoff for any repeat within this failure run.
            if consecutive >= PTY_GIVE_UP_CAP {
                let why = String::from_utf8_lossy(&recent);
                tracing::error!(
                    consecutive,
                    cooldown_s = PTY_COOLDOWN.as_secs(),
                    recent = %why.trim(),
                    "pty: tmux client keeps dying almost immediately (likely tmux < 3.2, a missing/broken tmux, or a spawn failure) — cooling down before another try; the FE LLM pane is degraded until this clears"
                );
                if sleep_unless_stopping(&stopping, PTY_COOLDOWN) {
                    break 'reader;
                }
            } else if consecutive > 0
                && sleep_unless_stopping(&stopping, respawn_backoff(consecutive))
            {
                break 'reader;
            }

            tracing::warn!(cols, rows, target = %cur_target, consecutive, "pty EOF — respawning tmux");
            match spawn_tmux_pair(cols, rows, &cur_target, cwd.as_deref(), slug.as_deref()) {
                Ok(TmuxPair {
                    master: new_master,
                    writer: new_writer,
                    child: new_child,
                    reader: new_reader,
                }) => {
                    if let Ok(mut g) = master.lock() {
                        *g = new_master;
                    }
                    if let Ok(mut g) = writer.lock() {
                        *g = new_writer;
                    }
                    tracing::info!(cols, rows, "pty respawn ok");
                    break 'respawn (new_child, new_reader);
                }
                Err(e) => {
                    // A spawn failure is also a short-lived failure — count it and
                    // retry after a paced delay rather than ending the reader.
                    consecutive += 1;
                    tracing::error!(error = %e, consecutive, "pty respawn failed; will retry after backoff");
                }
            }
        };

        child = new_child;
        reader = new_reader;
        spawned_at = Instant::now();
        recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_is_claude_matches_launcher_and_runtime() {
        // Claude Code reports as `claude` or `node`; the daemon-boot poll must
        // accept both (matches the FE-side `pane_command_is_claude` guard).
        assert!(pane_is_claude(Some("claude")));
        assert!(pane_is_claude(Some("node")));
    }

    #[test]
    fn parse_tmux_version_handles_real_shapes() {
        // The shapes tmux -V actually emits across the versions we care about.
        assert_eq!(parse_tmux_version("tmux 3.2\n"), Some((3, 2)));
        assert_eq!(parse_tmux_version("tmux 3.2a"), Some((3, 2)));
        assert_eq!(parse_tmux_version("tmux 3.0a\n"), Some((3, 0))); // the expectations box
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_tmux_version("tmux next-3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("tmux 2.9a"), Some((2, 9)));
        assert_eq!(parse_tmux_version("tmux 3"), Some((3, 0))); // missing minor -> 0
        // Fail-closed inputs: unparseable -> None (caller then omits -e).
        assert_eq!(parse_tmux_version("garbage"), None);
        assert_eq!(parse_tmux_version("tmux "), None);
        assert_eq!(parse_tmux_version(""), None);
    }

    #[test]
    fn tmux_dash_e_floor_is_3_2() {
        // The gate `tmux_supports_dash_e` applies: only >= 3.2 gets `-e`.
        let supports = |v: (u32, u32)| v >= (3, 2);
        assert!(supports((3, 2)));
        assert!(supports((3, 3)));
        assert!(supports((4, 0)));
        assert!(!supports((3, 0))); // 3.0a -> storm without the gate
        assert!(!supports((2, 9)));
    }

    #[test]
    fn pane_is_claude_rejects_shell_and_absent() {
        // A bash prompt (pre-launch / boot_failed) and an absent command must
        // NOT count as booted, or the poll would exit before claude is up.
        assert!(!pane_is_claude(Some("bash")));
        assert!(!pane_is_claude(Some("")));
        assert!(!pane_is_claude(None));
    }

    #[test]
    fn awareness_env_shapes() {
        // SOT_SESSION is always present and first; slug/cwd add their vars.
        // (SOT_MANUAL is checkout-dependent, so no assertion on it here.)
        let find = |env: &[(String, String)], k: &str| -> Option<String> {
            env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone())
        };
        let bare = awareness_env(None, None);
        assert_eq!(bare[0], ("SOT_SESSION".to_string(), "1".to_string()));
        assert_eq!(find(&bare, "SOT_WORKSPACE"), None);
        assert_eq!(find(&bare, "SOT_WORKSPACE_ROOT"), None);

        let full = awareness_env(Some("alpha"), Some(Path::new("/proj/alpha")));
        assert_eq!(find(&full, "SOT_WORKSPACE").as_deref(), Some("alpha"));
        assert_eq!(find(&full, "SOT_WORKSPACE_ROOT").as_deref(), Some("/proj/alpha"));
    }

    #[test]
    fn boot_wrapper_rereads_session_env_before_exec() {
        // The wrapper must re-read the tmux session env (per-name eval of
        // show-environment) AFTER the attach wait and BEFORE exec'ing the
        // launcher — that's what carries the SOT_* vars into the agent on
        // tmux < 3.2 (set-environment lands after the pane shell spawned).
        let w = boot_wrapper_command("sot-be-alpha", "alpha-agent", "claude");
        let eval_at = w
            .find("eval \"$(tmux show-environment -s -t 'sot-be-alpha'")
            .expect("wrapper re-reads session env");
        let name_at = w.find("export SOT_COMM_NAME=alpha-agent").expect("pins comm name");
        let exec_at = w.find("exec ~/.local/bin/ccb").expect("execs ccb");
        assert!(eval_at < name_at, "env re-read must precede the comm-name pin");
        assert!(name_at < exec_at, "comm-name pin must precede exec");
        // Lockstep pin: every var awareness_env can stamp must be in the
        // wrapper's exact-name re-read list (per-name query, never a prefix
        // grep — that was eval-injectable via hostile var names and broke
        // multiline values). A new awareness var without a wrapper entry
        // fails here.
        for (k, _) in awareness_env(Some("alpha"), Some(Path::new("/proj/alpha"))) {
            assert!(
                w.contains(&k),
                "awareness var {k} missing from the wrapper's re-read name list"
            );
        }
    }
}
