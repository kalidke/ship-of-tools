use super::*;

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
    assert_eq!(unix_shell(Some("/bin/sh")).program, "/bin/sh");
}
