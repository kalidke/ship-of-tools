// cli.rs — argv parsing for sot-frontend.
//
// Kept hand-rolled (no clap) because the deps tree is already heavy.
// `--help`/`-h` prints usage (issue #23 — it used to launch the GUI).

use std::path::PathBuf;

/// Split one `--demo-sessions` entry into `(slug, Option<state>)`. A bare
/// `foo` → `("foo", None)`; `foo:working` → `("foo", Some("working"))`. Only
/// the recognised work-states (working|idle|waiting|blocked|done) are kept as a
/// state; any other suffix is treated as part of the slug (None state) so a
/// stray colon doesn't silently swallow text. Returns `None` for an empty
/// slug so the caller can filter it out.
pub(crate) fn parse_demo_session(entry: &str) -> Option<(String, Option<String>)> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    match entry.split_once(':') {
        Some((slug, state))
            if !slug.is_empty()
                && matches!(state, "working" | "idle" | "waiting" | "blocked" | "done") =>
        {
            Some((slug.to_string(), Some(state.to_string())))
        }
        _ => Some((entry.to_string(), None)),
    }
}

#[derive(Debug, Clone)]
pub struct Cli {
    /// Backend local-socket path (Unix socket or Windows named pipe).
    /// `--socket` overrides `$SOT_SOCKET`.
    pub socket: Option<PathBuf>,
    /// Backend TCP address as `host:port` (per ADR 0010 cross-machine
    /// transport). `--tcp` overrides `$SOT_TCP`. When both `--socket` and
    /// `--tcp` are set, the pipe is tried first and TCP serves as the
    /// fallback on connect failure.
    pub tcp: Option<String>,
    /// App-level token sent in the `hello` handshake. Required when the
    /// backend is configured with `--token` / `$SOT_TOKEN`; mirror that
    /// here. `--token` overrides `$SOT_TOKEN`.
    pub token: Option<String>,
    /// If set, render to a PNG at this path and exit. Used as a headless test
    /// harness so changes to the rendering stack can be reviewed without an
    /// interactive eyeball pass.
    pub capture: Option<PathBuf>,
    /// Additional multiplier on text + cell metrics, on top of the OS DPR
    /// (`window.scale_factor()`). Default 1.0; bump it if the on-screen text
    /// is still too small or shrink it to fit more on screen.
    pub scale: f32,
    /// Which mode the chrome opens in. Mostly useful for the `--capture`
    /// path, where we can't inject `m`/`f` keystrokes mid-render. Accepts
    /// `files` or `modules`; defaults to `files`.
    pub start_mode: String,
    /// Initial cursor row in the TreeView, applied as soon as the first
    /// `tree.root` response lands. Like `--start-mode`, this is for the
    /// `--capture` path where we can't inject arrow keys. Out-of-range
    /// values clamp to the last row.
    pub start_selected: Option<usize>,
    /// If set, simulate one Enter/Right press on the cursored row after
    /// `--start-selected` lands. Lets `--capture` tests verify expansion
    /// (TreeChildren for files-mode dirs, FileParse for modules-mode
    /// rows) without needing real keyboard injection.
    pub auto_expand: bool,
    /// `<module>:<name>` — when the function row matching this id appears
    /// in the tree (after a prior module expansion has landed), move the
    /// cursor onto it and fire `function.methods`. Together with
    /// `--auto-expand` this lets a single `--capture` run exercise
    /// modules → definitions → methods (Modules-mode cols 1/2/3) without
    /// any keyboard injection.
    pub demo_function_methods: Option<(String, String)>,
    /// Files-mode analog of `--demo-function-methods`: a project-relative
    /// path to walk the cursor to. Collapsed ancestor directories are
    /// expanded one tree-update at a time, then the cursor lands ON the
    /// file's row — so the normal cursor-tracking passes (concept.read,
    /// file.parse, preview) fire exactly as they would for a user descent.
    /// `--capture-preview` can't do this: it only fires `preview.get`; the
    /// concept panel and drift badge key off the cursored row.
    pub start_path: Option<String>,
    /// Startup text/font scale (the Ctrl+= `text_scale_mult`, clamped to
    /// its 0.5–3.0 range). Takes precedence over the persisted fe-state
    /// restore, so a capture run renders at a PINNED size regardless of
    /// the box's local zoom or settings default — required for
    /// reproducible docs shots across capture boxes.
    pub font_scale: Option<f32>,
    /// One Julia expression to submit to the REPL at startup THROUGH the
    /// FE's own submit path (`submit_repl_input`), then open the REPL
    /// drawer. This is the only way a `--capture` run can show REPL
    /// output: the chrome renders `repl.frame` events only for evals whose
    /// eval_id matches a self-created `repl_log` entry — an eval submitted
    /// by an external protocol client is deliberately dropped, so a
    /// harness must go through the front door.
    pub demo_repl_eval: Option<String>,
    /// Open with the focused pane maximised (Alt+= behaviour applied at
    /// startup). For `--capture` harnesses that want a per-pane shot
    /// without injecting the keystroke mid-run. Pair with
    /// `--start-focus <nav|preview|llm|repl>` to choose which pane.
    pub start_maximized: bool,
    /// Open borderless-fullscreen on the active monitor at startup.
    /// Persisted-state fullscreen can't serve harness runs (`--capture` /
    /// `--ephemeral` must not depend on the box's saved geometry), and the
    /// docs screenshots are taken fullscreen on an ultrawide — the layout
    /// Ship of Tools is designed around.
    pub start_fullscreen: bool,
    /// Which pane has keyboard focus at startup. Useful with
    /// `--start-maximized` to capture each maximised pane in turn.
    /// Accepts `nav`, `preview`, `llm`, `repl`; defaults to `nav`.
    pub start_focus: String,
    /// Relative project path to fire `preview.get` for as soon as the
    /// initial files-mode `tree.root` lands. Lets `--capture` reach a
    /// file more than one level deep (e.g. `examples/preview/math_sample.md`)
    /// without keyboard injection.
    pub capture_preview: Option<String>,
    /// Override the `--capture` readback frame count, expressed in
    /// milliseconds at the redraw loop's nominal 60 Hz. Use this when
    /// the default wait (2 s with `--capture-preview`, 0.5 s without)
    /// isn't long enough for an asynchronous render to land — e.g. a
    /// markdown preview with `$$…$$` blocks pending on the MathJax
    /// sidecar. `0` keeps the default.
    pub capture_delay_ms: u32,
    /// Simulate N presses of the Shift+ArrowRight workspace cycle hotkey
    /// (D7) on the next `workspace.list` reply, before the capture
    /// readback. Negative values cycle backward (Shift+ArrowLeft). Lets the
    /// `--capture` harness verify a cycle without a key-injection rig.
    /// `0` (default) leaves the active workspace untouched.
    pub capture_cycle: i32,
    /// C2 pin-and-leave: simulate one press of `p` on the cursored row
    /// after the initial `tree.root` lands (so `--start-selected` has
    /// already moved the cursor). Lets the `--capture` harness verify
    /// the pin sigil and the `[pinned *]` preview-title chrome without
    /// keyboard injection. One-shot.
    pub auto_pin: bool,
    /// Open the `?` keybindings help overlay at startup. For the `--capture`
    /// harness, which can't inject a `?` keypress mid-render.
    pub start_help: bool,
    /// Open the Ctrl+M server-monitor drawer at startup (subscribed, with the
    /// default history prefill). For the `--capture` harness, which can't
    /// inject a Ctrl+M keypress mid-render.
    pub start_monitor: bool,
    /// This instance is a harness (driver/capture) FE, never the user's
    /// primary: skip every per-host shared-state interaction — no
    /// resume-state or `fe-state.json` writes, no state restore, and no
    /// consumption of the `fe-commands/` control directory. `--capture`
    /// implies the same skips. Single-writer rule for multi-FE hosts (B8).
    pub ephemeral: bool,
    /// Comma-separated session/workspace labels to seed offline, so the
    /// `--capture` harness can render the bottom session strip without a
    /// live backend. The middle entry is made active (centered). Ignored
    /// when a real `workspace.list` populates the strip. Each entry is an
    /// optional `slug:state` (state in working|idle|waiting|blocked|done) — when a
    /// `:state` suffix is present the slug seeds `workspace_states` so its
    /// work-state tone renders offline; a bare `slug` carries no state.
    pub demo_sessions: Vec<String>,
    /// Parallel to `demo_sessions`: the parsed `:state` of each entry, or
    /// `None` for a bare slug. Drives the offline `workspace_states` seed in
    /// the `--capture` harness so state-nav colours render without a backend.
    pub demo_session_states: Vec<Option<String>>,
    /// Comma-separated slugs to stamp a status-change *flash* on at startup,
    /// so `--capture` shows the flash near full brightness. Each listed slug
    /// gets `flash_starts[slug] = Instant::now()` plus a synthetic prior
    /// state so it reads as a real transition. Harness-only.
    pub demo_flash: Vec<String>,
    /// Selected-session contrast lever for state-nav (ADR 0023). `bright`
    /// (default) makes the selected/active row pop harder — brighter + bold —
    /// while leaving non-selected rows as they were. `dim` instead dims the
    /// *non*-selected rows so the selection pops by contrast. Runtime-
    /// selectable so each lever can be captured for comparison. Applies in
    /// both the nav rows and the bottom strip.
    pub contrast_mode: String,
    /// Set by the supervisor (`launch-sot.ps1`) when it respawns the
    /// frontend after a self-relaunch (exit code 75). Opens the Terminal
    /// drawer at startup and runs the configured `[terminal] resume_command`
    /// (default `claude --continue`) in it, so a `claude` session driving
    /// the rebuild loop reattaches itself in the fresh process. See ADR 0017.
    pub relaunched: bool,
}

impl Cli {
    pub fn parse() -> Self {
        let mut socket: Option<PathBuf> = None;
        let mut tcp: Option<String> = None;
        let mut token: Option<String> = None;
        let mut capture: Option<PathBuf> = None;
        let mut scale: f32 = 1.0;
        let mut start_mode: String = "files".to_string();
        let mut start_selected: Option<usize> = None;
        let mut auto_expand = false;
        let mut demo_function_methods: Option<(String, String)> = None;
        let mut start_maximized = false;
        let mut start_fullscreen = false;
        let mut start_path: Option<String> = None;
        let mut demo_repl_eval: Option<String> = None;
        let mut font_scale: Option<f32> = None;
        let mut start_focus: String = "nav".to_string();
        let mut capture_preview: Option<String> = None;
        let mut capture_delay_ms: u32 = 0;
        let mut capture_cycle: i32 = 0;
        let mut auto_pin = false;
        let mut start_help = false;
        let mut start_monitor = false;
        let mut ephemeral = false;
        let mut demo_sessions: Vec<String> = Vec::new();
        let mut demo_session_states: Vec<Option<String>> = Vec::new();
        let mut demo_flash: Vec<String> = Vec::new();
        let mut contrast_mode: String = "bright".to_string();
        let mut relaunched = false;

        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--version" | "-V" => {
                    println!("{}", sot_protocol::version_line("sot"));
                    std::process::exit(0);
                }
                // Issue #23: `sot --help` used to LAUNCH THE GUI in offline
                // sample mode — the flags were only discoverable from the
                // startup log. Print usage like a CLI and exit.
                "--help" | "-h" => {
                    println!("{}", sot_protocol::version_line("sot"));
                    println!(r#"
Usage: sot [OPTIONS]

Connection:
  --tcp <host:port>     connect to a backend over TCP (e.g. 127.0.0.1:18743)
  --socket <path>       connect over a unix socket / named pipe
  --token <token>       app-level auth token (must match the backend)
  (no connection flag)  offline sample mode with demo data

Display:
  --scale <f>           UI scale factor
  --font-scale <f>      font scale (0.5..3.0; overrides persisted/default)
  --contrast-mode       high-contrast rendering
  --start-fullscreen    start fullscreen      --start-maximized   start maximized
  --start-monitor <n>   pick monitor          --start-mode <m>    files|modules|sessions|hosts
  --start-path <p>      cursor a path         --start-help        open the ? overlay

Automation / dev (screenshots, demos):
  --capture <png> [--capture-delay-ms <ms>] [--capture-cycle] [--capture-preview]
  --demo-sessions <a:working,b:idle,...>      --demo-repl-eval <file>
  --ephemeral           don't persist state   --relaunched        (set by the supervisor)

The full keymap lives in the app: press ? — the nav pane header always
shows the pane-switch keys."#);
                    std::process::exit(0);
                }
                "--socket" => {
                    if let Some(v) = args.next() {
                        socket = Some(PathBuf::from(v));
                    }
                }
                "--tcp" => {
                    if let Some(v) = args.next() {
                        tcp = Some(v);
                    }
                }
                "--token" => {
                    if let Some(v) = args.next() {
                        token = Some(v);
                    }
                }
                "--capture" => {
                    if let Some(v) = args.next() {
                        capture = Some(PathBuf::from(v));
                    }
                }
                "--scale" => {
                    if let Some(v) = args.next() {
                        if let Ok(f) = v.parse::<f32>() {
                            if f > 0.1 && f < 10.0 {
                                scale = f;
                            }
                        }
                    }
                }
                "--start-mode" => {
                    if let Some(v) = args.next() {
                        if matches!(v.as_str(), "files" | "modules" | "sessions" | "hosts") {
                            start_mode = v;
                        }
                    }
                }
                "--start-selected" => {
                    if let Some(v) = args.next() {
                        if let Ok(n) = v.parse::<usize>() {
                            start_selected = Some(n);
                        }
                    }
                }
                "--auto-expand" => {
                    auto_expand = true;
                }
                "--demo-function-methods" => {
                    if let Some(v) = args.next() {
                        if let Some((m, n)) = v.split_once(':') {
                            if !m.is_empty() && !n.is_empty() {
                                demo_function_methods = Some((m.to_string(), n.to_string()));
                            }
                        }
                    }
                }
                "--start-maximized" => {
                    start_maximized = true;
                }
                "--start-fullscreen" => {
                    start_fullscreen = true;
                }
                "--start-path" => {
                    if let Some(v) = args.next() {
                        start_path = Some(v.replace('\\', "/"));
                    }
                }
                "--demo-repl-eval" => {
                    if let Some(v) = args.next() {
                        demo_repl_eval = Some(v);
                    }
                }
                "--font-scale" => {
                    if let Some(v) = args.next() {
                        font_scale = v.parse().ok();
                    }
                }
                "--start-focus" => {
                    if let Some(v) = args.next() {
                        if matches!(v.as_str(), "nav" | "preview" | "llm" | "repl") {
                            start_focus = v;
                        }
                    }
                }
                "--capture-preview" => {
                    if let Some(v) = args.next() {
                        capture_preview = Some(v.replace('\\', "/"));
                    }
                }
                "--capture-delay-ms" => {
                    if let Some(v) = args.next() {
                        if let Ok(n) = v.parse::<u32>() {
                            capture_delay_ms = n;
                        }
                    }
                }
                "--capture-cycle" => {
                    if let Some(v) = args.next() {
                        if let Ok(n) = v.parse::<i32>() {
                            capture_cycle = n;
                        }
                    }
                }
                "--auto-pin" => {
                    auto_pin = true;
                }
                "--start-help" => {
                    start_help = true;
                }
                "--start-monitor" => {
                    start_monitor = true;
                }
                "--ephemeral" => {
                    ephemeral = true;
                }
                "--demo-sessions" => {
                    if let Some(v) = args.next() {
                        let parsed: Vec<(String, Option<String>)> =
                            v.split(',').filter_map(parse_demo_session).collect();
                        demo_sessions = parsed.iter().map(|(s, _)| s.clone()).collect();
                        demo_session_states =
                            parsed.iter().map(|(_, st)| st.clone()).collect();
                    }
                }
                "--demo-flash" => {
                    if let Some(v) = args.next() {
                        demo_flash = v
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                    }
                }
                "--contrast-mode" => {
                    if let Some(v) = args.next() {
                        if matches!(v.as_str(), "bright" | "dim") {
                            contrast_mode = v;
                        }
                    }
                }
                "--relaunched" => {
                    relaunched = true;
                }
                _ => {
                    // Unknown args are ignored for now; emit a warning once
                    // `--help` exists so we don't silently mis-parse.
                }
            }
        }

        if socket.is_none() {
            if let Ok(v) = std::env::var("SOT_SOCKET") {
                socket = Some(PathBuf::from(v));
            }
        }
        if tcp.is_none() {
            if let Ok(v) = std::env::var("SOT_TCP") {
                tcp = Some(v);
            }
        }
        if token.is_none() {
            if let Ok(v) = std::env::var("SOT_TOKEN") {
                token = Some(v);
            }
        }

        Self {
            socket,
            tcp,
            token,
            capture,
            scale,
            start_mode,
            start_selected,
            auto_expand,
            demo_function_methods,
            start_maximized,
            start_fullscreen,
            start_path,
            demo_repl_eval,
            font_scale,
            start_focus,
            capture_preview,
            capture_delay_ms,
            capture_cycle,
            auto_pin,
            start_help,
            start_monitor,
            ephemeral,
            demo_sessions,
            demo_session_states,
            demo_flash,
            contrast_mode,
            relaunched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_demo_session;

    #[test]
    fn demo_session_parses_optional_state() {
        // Bare slug → no state.
        assert_eq!(parse_demo_session("alpha"), Some(("alpha".into(), None)));
        // slug:state for each recognised work-state.
        assert_eq!(
            parse_demo_session("beta:working"),
            Some(("beta".into(), Some("working".into())))
        );
        assert_eq!(
            parse_demo_session("gamma:blocked"),
            Some(("gamma".into(), Some("blocked".into())))
        );
        // Whitespace around an entry is trimmed.
        assert_eq!(
            parse_demo_session("  delta:done "),
            Some(("delta".into(), Some("done".into())))
        );
        // Unknown suffix is not a state — keep the whole thing as the slug so
        // a stray colon doesn't silently drop text.
        assert_eq!(
            parse_demo_session("eps:bogus"),
            Some(("eps:bogus".into(), None))
        );
        // Empty entry is filtered out.
        assert_eq!(parse_demo_session("   "), None);
    }
}
