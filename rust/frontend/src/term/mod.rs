// term/mod.rs — local terminal pane: PTY hosting + shell resolution.
//
// G2 (Frontend PTY plumbing): `LocalTerminal` spawns an arbitrary shell in a
// PTY, reads its output on a background thread forwarding chunks via mpsc, and
// feeds them into a vt100-ctt parser so the caller can blit the parsed screen.
//
// G5 (Per-OS shell selection): `resolve_shell` picks the best available shell
// on the current platform, honouring a user override from settings.
//
// The public API is marked `#[allow(dead_code)]` because the caller (the GPU
// render loop / pane manager) is wired in a separate step. Warnings on these
// items would be noise before that wiring lands.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

// ---------------------------------------------------------------------------
// Shell resolution (G5)
// ---------------------------------------------------------------------------

/// A resolved shell program and its argument list ready to pass to
/// `CommandBuilder`. On Unix, typical args are empty (the shell is its own
/// interactive mode). On Windows, `/K` or similar may be injected later by
/// the caller if needed — the resolver itself keeps args minimal.
#[allow(dead_code)]
pub struct ResolvedShell {
    pub program: String,
    pub args: Vec<String>,
}

/// Resolve the shell to spawn in the terminal pane.
///
/// Priority:
/// - If `override_` is `Some(s)` and `s` is non-empty, use it directly
///   (the caller is responsible for it being on PATH or an absolute path).
/// - Otherwise auto-detect per platform:
///   - **Unix**: `$SHELL` env var → `/bin/bash` → `/bin/sh`.
///   - **Windows**: `pwsh.exe` → `powershell.exe` → `cmd.exe`
///     (first found on `%PATH%`).
///
/// The fallback chain on Windows and the `$SHELL` env var on Unix each
/// guarantee a non-empty `program`; the final `/bin/sh` / `cmd.exe`
/// candidates are always present on their respective platforms.
#[allow(dead_code)]
pub fn resolve_shell(override_: Option<&str>) -> ResolvedShell {
    if let Some(s) = override_ {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return ResolvedShell {
                program: trimmed.to_string(),
                args: vec![],
            };
        }
    }
    resolve_shell_auto()
}

#[cfg(unix)]
fn resolve_shell_auto() -> ResolvedShell {
    // $SHELL is the authoritative user-preference on Unix.
    if let Ok(shell) = std::env::var("SHELL") {
        let s = shell.trim().to_string();
        if !s.is_empty() {
            return ResolvedShell { program: s, args: vec![] };
        }
    }
    // Fallback chain: bash is ubiquitous; sh is POSIX-guaranteed.
    for candidate in &["/bin/bash", "/bin/sh"] {
        if std::path::Path::new(candidate).exists() {
            return ResolvedShell {
                program: candidate.to_string(),
                args: vec![],
            };
        }
    }
    // Last-ditch: /bin/sh should always exist on any Unix, but if somehow
    // neither file exists we still return something rather than panic.
    ResolvedShell { program: "/bin/sh".to_string(), args: vec![] }
}

#[cfg(windows)]
fn resolve_shell_auto() -> ResolvedShell {
    // Probe PATH for each candidate in priority order.
    for candidate in &["pwsh.exe", "powershell.exe", "cmd.exe"] {
        if program_on_path(candidate) {
            return ResolvedShell {
                program: candidate.to_string(),
                args: vec![],
            };
        }
    }
    // cmd.exe is always present on Windows; return it even if PATH probe
    // failed (it may not be on PATH but the OS can still find it).
    ResolvedShell { program: "cmd.exe".to_string(), args: vec![] }
}

/// Build the argument list that makes `shell` run `command` on startup and
/// then stay interactive. Used by the `--relaunched` resume path so a
/// `claude --continue` (or any configured `resume_command`) reattaches in
/// the freshly spawned terminal.
///
/// Injecting via shell args (rather than writing to the PTY's stdin after
/// spawn) sidesteps the timing race where input arrives before the shell's
/// first prompt is ready — the shell parses these at launch.
///
/// - PowerShell / pwsh: `-NoExit -Command <cmd>` (run, then drop to prompt).
/// - cmd.exe:           `/K <cmd>` (run, then keep the session).
/// - POSIX shells:      `-c "<cmd>; exec <shell>"` (run, then replace the
///   process with a fresh interactive shell so the pane stays usable).
#[allow(dead_code)]
fn resume_command_args(program: &str, command: &str) -> Vec<String> {
    let prog_lower = program.to_ascii_lowercase();
    let base = std::path::Path::new(&prog_lower)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&prog_lower);
    if base.contains("powershell") || base.contains("pwsh") {
        vec!["-NoExit".into(), "-Command".into(), command.into()]
    } else if base.contains("cmd") {
        vec!["/K".into(), command.into()]
    } else {
        // POSIX: run the command, then exec an interactive shell so the
        // terminal doesn't exit when the command returns.
        vec!["-c".into(), format!("{command}; exec {program}")]
    }
}

/// Minimal PATH probe: check whether `name` exists as an executable on
/// the current `PATH`. We avoid the `which` crate to keep the dep graph
/// lean — this is a simple existence check, not full `execvp` resolution.
#[cfg(windows)]
fn program_on_path(name: &str) -> bool {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Local terminal (G2)
// ---------------------------------------------------------------------------

/// A locally-spawned shell session backed by a PTY.
///
/// Lifecycle:
/// 1. Call `spawn` — opens a PTY pair, launches the shell, starts a reader
///    thread that forwards raw bytes via mpsc and calls `wake()`.
/// 2. On each redraw: call `pump()` to drain pending bytes into the vt100
///    parser. Returns `true` if anything was consumed (caller schedules
///    a repaint).
/// 3. Call `screen()` to get the parsed terminal grid for rendering.
/// 4. Call `send_input` to forward keystrokes to the shell's stdin.
/// 5. Call `resize` when the pane rect changes.
/// 6. Poll `is_dead()` to detect shell exit.
#[allow(dead_code)]
pub struct LocalTerminal {
    /// Owns the PTY master so we can resize and keep the slave alive.
    /// Behind a `Box` because `MasterPty` is not `Sized`.
    master: Box<dyn MasterPty + Send>,
    /// The spawned shell process. **Must be kept alive** — on Windows
    /// (ConPTY) dropping the child handle terminates the process, which
    /// would leave a blank, unresponsive pane. Held for its whole life;
    /// `is_dead` polls it via `try_wait`.
    child: Box<dyn Child + Send + Sync>,
    /// Write half of the PTY (shell stdin). Separate from master because
    /// `portable-pty`'s `take_writer` consumes the master's write end.
    writer: Box<dyn Write + Send>,
    /// vt100 parser accumulating the shell's output.
    parser: vt100::Parser,
    /// Channel receiving raw byte chunks from the reader thread.
    rx: Receiver<Vec<u8>>,
    /// Set by the reader thread when the child exits or the PTY closes.
    dead: Arc<AtomicBool>,
    /// Join handle for the reader thread (held so the thread is not
    /// detached; we never explicitly join but the handle keeps it named).
    #[allow(dead_code)]
    _reader_thread: thread::JoinHandle<()>,
}

impl LocalTerminal {
    /// Spawn `shell` in a PTY sized `cols × rows`.
    ///
    /// `cwd` sets the shell's working directory (e.g. the repo root so
    /// `claude --continue` resumes the right project's session); `None`
    /// inherits the frontend's cwd. `initial_command`, when set, is run on
    /// startup via [`resume_command_args`] and the shell then stays
    /// interactive.
    ///
    /// `wake` is called by the reader thread after each chunk arrives so
    /// the UI event loop can schedule a redraw without polling.
    #[allow(dead_code)]
    pub fn spawn(
        shell: &ResolvedShell,
        cols: u16,
        rows: u16,
        cwd: Option<&std::path::Path>,
        initial_command: Option<&str>,
        wake: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Self> {
        let rows = rows.max(1);
        let cols = cols.max(1);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty: {e}"))?;

        let mut cmd = CommandBuilder::new(&shell.program);
        // An initial command replaces the plain interactive invocation with
        // a "run-then-stay-interactive" arg form; otherwise use the shell's
        // own (typically empty) args.
        match initial_command {
            Some(c) if !c.trim().is_empty() => {
                for arg in resume_command_args(&shell.program, c.trim()) {
                    cmd.arg(arg);
                }
            }
            _ => {
                for arg in &shell.args {
                    cmd.arg(arg);
                }
            }
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Advertise ourselves as a 256-colour terminal so the shell and
        // apps running inside it use colour escape sequences.
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("spawn shell in pty")?;
        // Drop the slave handle — the child process owns it now. Keep the
        // `child` handle (see field doc: dropping it kills the shell on
        // Windows ConPTY).
        drop(pair.slave);

        let master: Box<dyn MasterPty + Send> = pair.master;

        // Clone a reader before taking the writer, because `take_writer`
        // consumes the write end and the reader is cloned from the master.
        let reader: Box<dyn Read + Send> = master
            .try_clone_reader()
            .context("clone pty reader")?;
        let writer: Box<dyn Write + Send> = master
            .take_writer()
            .context("take pty writer")?;

        let parser = vt100::Parser::new(rows, cols, 5000);

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let dead = Arc::new(AtomicBool::new(false));
        let dead_for_thread = Arc::clone(&dead);

        let reader_thread = thread::Builder::new()
            .name("sot-term-reader".to_string())
            .spawn(move || {
                run_reader(reader, tx, dead_for_thread, wake);
            })
            .context("spawn term reader thread")?;

        Ok(Self {
            master,
            child,
            writer,
            parser,
            rx,
            dead,
            _reader_thread: reader_thread,
        })
    }

    /// Drain all pending byte chunks from the reader channel into the vt100
    /// parser (non-blocking). Returns `true` if at least one chunk was
    /// processed — the caller should schedule a repaint.
    #[allow(dead_code)]
    pub fn pump(&mut self) -> bool {
        let mut processed = false;
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.parser.process(&chunk);
                    // ConPTY (and full-screen apps) emit ANSI *query*
                    // sequences and block until the terminal answers. Must
                    // run after process() so the cursor position we report
                    // reflects the bytes preceding the query.
                    self.respond_to_queries(&chunk);
                    processed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        processed
    }

    /// Answer the standard ANSI device queries a host sends during startup.
    ///
    /// This is load-bearing on Windows: ConPTY leads its handshake with a
    /// DSR cursor-position report request (`ESC[6n`) and **does not print the
    /// shell prompt until it receives the reply**. A bare `vt100::Parser`
    /// never responds, so without this the Windows terminal pane stays blank
    /// forever (shell alive, zero visible output) — the exact symptom we hit.
    /// Unix shells don't open with this handshake, which is why it only bit
    /// on Windows.
    ///
    /// We answer:
    /// - `ESC[6n` (DSR cursor position) → `ESC[<row>;<col>R` (1-based),
    /// - `ESC[5n` (DSR device status)   → `ESC[0n` ("OK").
    ///
    /// Both query forms are a fixed 4 bytes, so a per-chunk window scan is
    /// reliable: ConPTY writes each query as its own small chunk, and the
    /// 4-byte sequences don't straddle our 8 KiB reads in practice.
    fn respond_to_queries(&mut self, chunk: &[u8]) {
        let mut reply: Vec<u8> = Vec::new();
        for w in chunk.windows(4) {
            if w == b"\x1b[6n" {
                let (row, col) = self.parser.screen().cursor_position();
                reply.extend_from_slice(
                    format!("\x1b[{};{}R", row as usize + 1, col as usize + 1).as_bytes(),
                );
            } else if w == b"\x1b[5n" {
                reply.extend_from_slice(b"\x1b[0n");
            }
        }
        if reply.is_empty() {
            return;
        }
        if let Err(e) = self.writer.write_all(&reply) {
            tracing::warn!(error = %e, "term: query reply write failed");
        } else if let Err(e) = self.writer.flush() {
            tracing::warn!(error = %e, "term: query reply flush failed");
        }
    }

    /// Current parsed terminal screen. Pass to the renderer.
    #[allow(dead_code)]
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Mutable screen access — used to drive `set_scrollback` for the
    /// drawer's scrollback offset before the immutable draw borrow.
    #[allow(dead_code)]
    pub fn screen_mut(&mut self) -> &mut vt100::Screen {
        self.parser.screen_mut()
    }

    /// True when the running app enabled mouse reporting (e.g. vim, less,
    /// htop). The drawer forwards wheel events to such apps as SGR mouse
    /// sequences instead of scrolling our own ring (which they bypass with
    /// cursor-positioned redraws). Plain shells leave this `false`.
    #[allow(dead_code)]
    pub fn mouse_tracking_on(&self) -> bool {
        !matches!(
            self.parser.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        )
    }

    /// Forward raw keystroke bytes to the shell's stdin.
    #[allow(dead_code)]
    pub fn send_input(&mut self, bytes: &[u8]) {
        // Best-effort: log on error but don't propagate (the render loop
        // is not in a position to handle a write error gracefully, and
        // the dead flag will surface the problem on the next pump()).
        if let Err(e) = self.writer.write_all(bytes) {
            tracing::warn!(error = %e, "term: send_input write_all failed");
        } else if let Err(e) = self.writer.flush() {
            tracing::warn!(error = %e, "term: send_input flush failed");
        }
    }

    /// Resize both the PTY (sends SIGWINCH to the child) and the vt100
    /// parser so their grids stay in sync.
    #[allow(dead_code)]
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if let Err(e) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            tracing::warn!(error = %e, "term: pty resize failed");
        }
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Returns `true` once the child shell has exited or the PTY has closed.
    /// Checked by the pane manager to show a "Process exited" overlay.
    /// Polls the child handle (non-blocking) in addition to the reader
    /// thread's EOF/error flag.
    #[allow(dead_code)]
    pub fn is_dead(&mut self) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(_status)) => {
                self.dead.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Reader thread
// ---------------------------------------------------------------------------

/// Blocking loop that drains the PTY master reader and forwards chunks over
/// `tx`. Calls `wake()` after each successful read to trigger a UI redraw.
/// Sets `dead` and exits on EOF (`Ok(0)`) or read error.
fn run_reader(
    mut reader: Box<dyn Read + Send>,
    tx: mpsc::Sender<Vec<u8>>,
    dead: Arc<AtomicBool>,
    wake: Box<dyn Fn() + Send + 'static>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                // EOF — child exited or PTY closed.
                tracing::debug!("term reader: EOF, marking dead");
                dead.store(true, Ordering::Relaxed);
                wake();
                break;
            }
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    // Receiver (LocalTerminal) was dropped.
                    break;
                }
                wake();
            }
            Err(e) => {
                tracing::warn!(error = %e, "term reader: read error, stopping");
                dead.store(true, Ordering::Relaxed);
                wake();
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_command_args_per_shell() {
        // PowerShell family → -NoExit -Command <cmd>.
        assert_eq!(
            resume_command_args("powershell.exe", "claude --continue"),
            vec!["-NoExit", "-Command", "claude --continue"]
        );
        assert_eq!(
            resume_command_args("C:\\Program Files\\PowerShell\\7\\pwsh.exe", "x"),
            vec!["-NoExit", "-Command", "x"]
        );
        // cmd.exe → /K <cmd>.
        assert_eq!(
            resume_command_args("cmd.exe", "claude --continue"),
            vec!["/K", "claude --continue"]
        );
        // POSIX → -c "<cmd>; exec <shell>".
        assert_eq!(
            resume_command_args("/bin/bash", "claude --continue"),
            vec!["-c", "claude --continue; exec /bin/bash"]
        );
    }

    #[test]
    fn resolve_shell_honours_override() {
        // An explicit override is returned verbatim (no PATH check).
        let s = resolve_shell(Some("/usr/bin/fish"));
        assert_eq!(s.program, "/usr/bin/fish");
        assert!(s.args.is_empty());
    }

    #[test]
    fn resolve_shell_auto_returns_nonempty_program() {
        // On any platform the auto-resolver must return a non-empty program.
        let s = resolve_shell(None);
        assert!(!s.program.is_empty(), "auto-resolved shell must be non-empty");
    }

    #[test]
    fn resolve_shell_empty_override_falls_through_to_auto() {
        // Empty-string and whitespace-only overrides should be ignored.
        let empty = resolve_shell(Some(""));
        let whitespace = resolve_shell(Some("   "));
        let auto = resolve_shell(None);
        assert_eq!(empty.program, auto.program);
        assert_eq!(whitespace.program, auto.program);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_echo_roundtrips_through_screen() {
        // End-to-end smoke test: spawn a real shell, send a command, and
        // confirm its output lands in the parsed screen. Exercises
        // spawn → reader thread → mpsc → pump → parser → screen and
        // send_input → pty stdin. Unix-only (uses /bin/sh).
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let woke = Arc::new(AtomicBool::new(false));
        let woke_c = Arc::clone(&woke);
        let shell = ResolvedShell { program: "/bin/sh".to_string(), args: vec![] };
        let mut term = LocalTerminal::spawn(
            &shell,
            80,
            24,
            None,
            None,
            Box::new(move || woke_c.store(true, Ordering::Relaxed)),
        )
        .expect("spawn /bin/sh");

        term.send_input(b"echo SOT_MARKER_OK\n");

        // Poll up to ~3s for the marker to appear in the rendered screen.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            term.pump();
            let screen = term.screen();
            let (rows, cols) = screen.size();
            let mut text = String::new();
            for r in 0..rows {
                for c in 0..cols {
                    if let Some(cell) = screen.cell(r, c) {
                        text.push_str(cell.contents());
                    }
                }
                text.push('\n');
            }
            if text.contains("SOT_MARKER_OK") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "echo output never reached the parsed screen");
        assert!(woke.load(Ordering::Relaxed), "wake() was never called");
    }

    #[cfg(windows)]
    #[test]
    fn spawn_windows_shell_renders_prompt_after_dsr_reply() {
        // Windows regression test for the ConPTY DSR handshake. ConPTY leads
        // with `ESC[6n` and withholds the shell prompt until the terminal
        // replies; `respond_to_queries` (driven by `pump`) supplies that
        // reply. Without the fix this pane stays blank forever. We assert the
        // banner/prompt reaches the parsed screen, then round-trip an echoed
        // marker. Uses cmd.exe (always present, no profile-load latency).
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let woke = Arc::new(AtomicBool::new(false));
        let woke_c = Arc::clone(&woke);
        let shell = ResolvedShell { program: "cmd.exe".to_string(), args: vec![] };
        let mut term = LocalTerminal::spawn(
            &shell,
            80,
            24,
            None,
            None,
            Box::new(move || woke_c.store(true, Ordering::Relaxed)),
        )
        .expect("spawn cmd.exe");

        let screen_text = |term: &LocalTerminal| -> String {
            let screen = term.screen();
            let (rows, cols) = screen.size();
            let mut text = String::new();
            for r in 0..rows {
                for c in 0..cols {
                    if let Some(cell) = screen.cell(r, c) {
                        text.push_str(cell.contents());
                    }
                }
                text.push('\n');
            }
            text
        };

        // Banner/prompt must appear — proves the DSR reply unblocked ConPTY.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut banner = false;
        while std::time::Instant::now() < deadline {
            term.pump();
            if screen_text(&term).trim().chars().any(|ch| !ch.is_whitespace()) {
                banner = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            banner,
            "no banner/prompt reached the screen — DSR handshake not answered (dead={})",
            term.is_dead()
        );

        // Round-trip an echoed marker through stdin → shell → screen.
        term.send_input(b"echo SOT_MARKER_OK\r\n");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            term.pump();
            if screen_text(&term).contains("SOT_MARKER_OK") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "echo output never reached the parsed screen");
        assert!(woke.load(Ordering::Relaxed), "wake() was never called");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_shell_unix_prefers_shell_env() {
        // Temporarily set $SHELL and verify it is used.
        let old = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/bin/sh");
        let s = resolve_shell(None);
        // Restore before asserting so a failure doesn't leave state dirty.
        match old {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        assert_eq!(s.program, "/bin/sh");
    }
}
