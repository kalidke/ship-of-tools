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
use std::sync::{Arc, Mutex};
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
    /// Held so the child stays alive — drops when the Pty drops, or
    /// when the reader thread swaps in a respawned tmux child (the
    /// old box's drop reaps the dead one). The mutex isn't read
    /// by anyone else but keeping it Arc<Mutex<>> matches the other
    /// two slots, so the swap path is uniform.
    #[allow(dead_code)]
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
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
        let child = Arc::new(Mutex::new(child));
        let target = Arc::new(Mutex::new(target_name));
        let size = Arc::new(Mutex::new((cols.max(1), rows.max(1))));
        let watch = Arc::new(Mutex::new(ResizeWatch::default()));
        let stopping = Arc::new(AtomicBool::new(false));

        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let master_for_reader = Arc::clone(&master);
        let writer_for_reader = Arc::clone(&writer);
        let child_for_reader = Arc::clone(&child);
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
                master_for_reader,
                writer_for_reader,
                child_for_reader,
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
            child,
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
    // Ship of Tools awareness env, set via `-e` so it lands in the NEW session's
    // environment. A child-process env var (cmd.env, below for TERM) does NOT
    // propagate to a session created under an already-running tmux server —
    // tmux derives the new session's env from the server and copies only the
    // `update-environment` allowlist from the connecting client, so an
    // arbitrary var is dropped. `-e` sets it on the session explicitly.
    // Honoured on create; `-A` ignores it when attaching (same as `-c`), so it
    // lands on freshly created workspace sessions. A session in the pane reads
    // these to detect it is inside Ship of Tools, which workspace it is in, and to
    // drive the FE (open nav / preview).
    cmd.arg("-e");
    cmd.arg("SOT_SESSION=1");
    if let Some(slug) = slug {
        cmd.arg("-e");
        cmd.arg(format!("SOT_WORKSPACE={slug}"));
    }
    if let Some(dir) = cwd {
        cmd.arg("-e");
        cmd.arg(format!("SOT_WORKSPACE_ROOT={}", dir.to_string_lossy()));
    }
    // Clone-based install (ADR 0030 addendum): point the pane's agent at the
    // product's own checkout — the repo IS the manual, and docs/USING.md is
    // its entry point for a help+extend persona. Resolved through the same
    // chain as every other resource, so dev checkouts get the dev tree and
    // installs get $PREFIX/repo/current. Only exported when it exists.
    let manual = crate::paths::resource_dir("docs");
    if manual.exists() {
        if let Some(root) = manual.parent() {
            cmd.arg("-e");
            cmd.arg(format!("SOT_MANUAL={}", root.to_string_lossy()));
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

    let master: Box<dyn MasterPty + Send> = pair.master;
    let reader = master
        .try_clone_reader()
        .map_err(|e| anyhow!("clone pty reader: {e}"))?;
    let writer = master
        .take_writer()
        .map_err(|e| anyhow!("take pty writer: {e}"))?;
    Ok(TmuxPair { master, writer, child, reader })
}

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
    // the session was already rooted + env-stamped at create time.
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
    let TmuxPair { master, writer, child, reader } = pair;

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
    drop(child);
    let _ = drain.join();

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

/// Reader-thread loop. Drains the master reader into the byte channel.
/// On `Ok(0)` (child exit / tmux server gone) it spawns a fresh tmux
/// pair sized to the latest cached dims, swaps it into the shared
/// master/writer/child slots, takes the new reader, and keeps going.
/// If the respawn itself fails we log + exit the loop — the next
/// `pty.write` from the server task will then fail with "channel
/// closed" and surface the breakage to the frontend.
///
/// `stopping`, when set by `Pty::shutdown` / `Drop`, means the EOF we are about
/// to observe is a deliberate teardown (re-target / connection close) — break
/// out and let the master fd close instead of respawning. This is what lets the
/// left-behind session be released cleanly without the reader re-attaching to it.
#[allow(clippy::too_many_arguments)]
fn run_reader_loop(
    mut reader: Box<dyn Read + Send>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    target: Arc<Mutex<String>>,
    size: Arc<Mutex<(u16, u16)>>,
    stopping: Arc<AtomicBool>,
    // Project root to recreate the session in (`-c <cwd>`) on respawn.
    // Cleared to `None` if we fall back to the home-base default below,
    // since that session is deliberately not workspace-rooted.
    mut cwd: Option<PathBuf>,
    // Owning workspace slug (`SOT_WORKSPACE`), same lifecycle as `cwd`:
    // cleared on the home-base fallback so a respawn carries no stale id.
    mut slug: Option<String>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                // Deliberate teardown (re-target / Pty drop): the client was
                // detached on purpose, so don't respawn — just exit and let the
                // master fd close. The session is left intact for re-attach.
                if stopping.load(Ordering::SeqCst) {
                    tracing::info!("pty EOF during shutdown; stopping reader (no respawn)");
                    break;
                }
                // EOF — child exited or tmux server died. Try to come
                // back up with a fresh pair at the last known size,
                // against the current target (Sessions mode may have
                // re-targeted us between the start of the loop and now).
                let (cols, rows) = match size.lock() {
                    Ok(g) => *g,
                    Err(_) => {
                        tracing::error!("size mutex poisoned; cannot respawn pty");
                        break;
                    }
                };
                let target_name = match target.lock() {
                    Ok(g) => g.clone(),
                    Err(_) => {
                        tracing::error!("target mutex poisoned; cannot respawn pty");
                        break;
                    }
                };
                // Never resurrect a dead NAMED session: `new-session -A`
                // against a name that no longer exists CREATES it — bare,
                // cwd=~, no `-c`. When the EOF came from `workspace.destroy`
                // killing the session under an attached client, that
                // resurrection races the follow-up `workspace.create` into
                // "duplicate session", registering a workspace without its
                // owned session (observed live 2026-06-11 20:51Z). Re-attach
                // only if the session still exists; otherwise fall back to
                // the home-base default — recreating THAT is the original
                // recovery semantics (e.g. whole tmux-server death), and the
                // target slot is updated so re-target comparisons stay honest.
                let target_name = if target_name != DEFAULT_TMUX_TARGET
                    && !session_exists(&target_name)
                {
                    tracing::warn!(gone = %target_name,
                        "pty EOF — target session no longer exists; falling back to home-base instead of resurrecting it");
                    if let Ok(mut g) = target.lock() {
                        *g = DEFAULT_TMUX_TARGET.to_string();
                    }
                    // Home-base is deliberately not workspace-rooted; drop
                    // the project cwd (and workspace slug) so the fallback
                    // create doesn't pin it to a workspace it no longer
                    // represents.
                    cwd = None;
                    slug = None;
                    DEFAULT_TMUX_TARGET.to_string()
                } else {
                    target_name
                };
                tracing::warn!(cols, rows, target = %target_name, "pty EOF — respawning tmux");
                match spawn_tmux_pair(cols, rows, &target_name, cwd.as_deref(), slug.as_deref()) {
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
                        if let Ok(mut g) = child.lock() {
                            *g = new_child;
                        }
                        reader = new_reader;
                        tracing::info!(cols, rows, "pty respawn ok");
                    }
                    Err(e) => {
                        tracing::error!(error = %e,
                            "pty respawn failed; stopping reader");
                        break;
                    }
                }
            }
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break; // receiver dropped, stop reading
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "pty read error; stopping reader");
                break;
            }
        }
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
    fn pane_is_claude_rejects_shell_and_absent() {
        // A bash prompt (pre-launch / boot_failed) and an absent command must
        // NOT count as booted, or the poll would exit before claude is up.
        assert!(!pane_is_claude(Some("bash")));
        assert!(!pane_is_claude(Some("")));
        assert!(!pane_is_claude(None));
    }
}
