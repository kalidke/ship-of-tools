//! `State::new`: the launch sequence that builds the window's state, one step per function, in order.

use super::*;

mod fields;
use fields::StartupParts;
use super::render::surface::{
    build_solid_quads, build_text_grid, create_gpu_surface, decode_logo_quads, GpuSurface, LogoQuads,
    SolidQuads, TextGrid,
};

impl State {
    pub(super) fn new(
        event_loop: &ActiveEventLoop,
        evt_rx: std::sync::mpsc::Receiver<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>,
        cli: &crate::cli::Cli,
        conns: Vec<(
            crate::net::dial::HostKey,
            tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
        )>,
        leases: Arc<crate::lease::Leases>,
    ) -> Result<Self> {
        let launch = load_launch_inputs(&conns);
        let window =
            create_window(event_loop, cli, &launch.persisted_geom, launch.init_w, launch.init_h)?;
        let metrics = compute_cell_metrics(cli, &window);
        let gpu = create_gpu_surface(&window, &launch.settings)?;
        let mut text_grid =
            build_text_grid(&gpu.device, &gpu.queue, &gpu.config, gpu.surface_format, metrics)?;
        let solid = build_solid_quads(&gpu.device, &gpu.queue, gpu.surface_format)?;
        let logos = decode_logo_quads(&gpu.device, &gpu.queue, &solid.quad_pipeline);
        let preview_png = load_splash_png(&gpu.device, &gpu.queue, &solid.quad_pipeline);
        let content = build_preview_content(&mut text_grid.text, &gpu.config, metrics.scale)?;

        // Self-relaunch wiring (ADR 0017). `$SOT_REPO_DIR` is set by the
        // supervisor and points at the local repo root; the Terminal drawer
        // uses it as its cwd. The ADR-0017 supervisor exports SOT_REPO_DIR so
        // the drawer's shell lands in the repo dir rather than $HOME (the
        // wrong-folder relaunch, observed 2026-06-25).
        //
        // Open the drawer on startup when resuming a self-relaunch
        // (`--relaunched`) — it runs a plain shell (ADR 0041/0042 retired
        // the resume-command ritual; the frontend driver now lives in its
        // own local capsule session, not this drawer). With no relaunch,
        // the FE opens clean (drawer closed).
        let repo_dir = std::env::var_os("SOT_REPO_DIR").map(std::path::PathBuf::from);
        let harness = cli.ephemeral || cli.capture.is_some();
        let want_terminal_init = !harness && cli.relaunched;

        let parts = StartupParts {
            evt_rx,
            conns,
            leases,
            launch,
            window,
            metrics,
            gpu,
            text_grid,
            solid,
            logos,
            preview_png,
            content,
            repo_dir,
            harness,
            want_terminal_init,
        };
        let mut state = Self::from_parts(event_loop, cli, parts);
        // Self-update notice from the launcher's own process spawn (a
        // REFUSED pull; empty/unset for offline or ok - see
        // scripts/launch-sot.ps1). Same two fields the FeCommand::Notify
        // arm below writes - no separate renderer.
        if let Ok(notice) = std::env::var("SOT_LAUNCH_NOTICE") {
            if !notice.is_empty() {
                state.status = notice;
                state.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
            }
        }
        apply_harness_flags(&mut state, cli);
        apply_startup_font_scale(&mut state, cli);
        Ok(state)
    }
}

struct LaunchInputs {
    persisted_geom: crate::ui::persist::resume::GlobalState,
    active_host: crate::net::dial::HostKey,
    resume_matches_last_host: bool,
    monitor_hub: Option<String>,
    init_w: f64,
    init_h: f64,
    settings: Settings,
}

fn load_launch_inputs(
    conns: &[(crate::net::dial::HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
) -> LaunchInputs {
    #[cfg(all(test, feature = "test-window-progress"))]
    if let Some(inputs) = NATIVE_STARTUP.with(|slot| slot.borrow().clone()) {
        let persisted_geom = inputs.resume;
        let active_host = persisted_geom
            .last_host
            .clone()
            .filter(|h| conns.iter().any(|(ch, _)| ch == h))
            .unwrap_or_else(|| resolve_default_host(conns, "offline".to_string()));
        return LaunchInputs {
            resume_matches_last_host: persisted_geom.last_host.as_ref() == Some(&active_host),
            init_w: persisted_geom.window_w.unwrap_or(640.0),
            init_h: persisted_geom.window_h.unwrap_or(480.0),
            persisted_geom,
            active_host,
            monitor_hub: inputs.topology.map(|t| t.hub),
            settings: inputs.settings,
        };
    }
    // Loaded here (rather than at each of its several uses below) so
    // `last_host` and the window-geometry fields below all read the
    // SAME snapshot of the file.
    let persisted_geom = crate::ui::persist::resume::load();
    // ADR 0042 L2a codex review, item H: `last_host` is the active
    // host AT QUIT (persist_resume_state writes it every save now —
    // see the field's own doc for the ADR 0015 -> L2a meaning
    // change). It wins whenever it's still a resolved connection, so a
    // daily launch resumes wherever the user actually left off;
    // `resolve_default_host` (G's rule — no more configured
    // `default_host` since topology plan lane D, so this always falls
    // back to `conns.first()`) is the fallback when there's no
    // persisted host, or it's no longer reachable.
    let active_host: crate::net::dial::HostKey = persisted_geom
        .last_host
        .clone()
        .filter(|h| conns.iter().any(|(ch, _)| ch == h))
        .unwrap_or_else(|| resolve_default_host(&conns, "offline".to_string()));
    // ADR 0042 L2a codex review, item H: `last_workspace_id` /
    // `last_bl_target` were saved for WHATEVER host was active at
    // quit. If that host is unreachable now and `active_host` fell
    // back to G's rule (a DIFFERENT host), those saved values name a
    // workspace/session that may not exist -- or worse, exist under
    // the SAME name -- on the fallback host. Restore them only when
    // we actually resumed onto the host they were saved for.
    let resume_matches_last_host =
        persisted_geom.last_host.as_deref() == Some(active_host.as_str());
    // The declared hub's name (4.3): loaded once here, the same pattern
    // `selfupdate.rs::backend_owns_updates_here` uses — pure file read,
    // no daemon round trip, `None` when no `hosts.toml` declares one.
    let monitor_hub: Option<String> =
        sot_protocol::topology::load().ok().flatten().map(|(_, t)| t.hub);
    // Restore previous window geometry on launch. Saved in logical
    // pixels so cross-DPR launches behave sensibly. Defaults are
    // ~50% bigger than the spike's original 1024×700.
    let init_w = persisted_geom.window_w.unwrap_or(1536.0);
    let init_h = persisted_geom.window_h.unwrap_or(1050.0);
    // Loaded here rather than at first use (the Terminal-drawer resume,
    // far below) because adapter selection needs `[gpu] power_preference`
    // and that happens a few dozen lines down. Pure env+fs, no dependency
    // on the window or event loop. ONE load — a second call would log
    // "settings loaded" twice and could disagree if the file changed
    // mid-startup.
    let settings = Settings::load_layered();
    LaunchInputs { persisted_geom, active_host, resume_matches_last_host, monitor_hub, init_w, init_h, settings }
}

fn load_keybindings() -> KeyBindings {
    #[cfg(all(test, feature = "test-window-progress"))]
    if let Some(bindings) =
        NATIVE_STARTUP.with(|slot| slot.borrow().as_ref().map(|i| i.keybindings.clone()))
    {
        return bindings;
    }
    KeyBindings::load_layered()
}

#[cfg(all(test, feature = "test-window-progress"))]
#[derive(Clone)]
pub(in crate::ui) struct NativeStartupInputs {
    pub(in crate::ui) resume: crate::ui::persist::resume::GlobalState,
    pub(in crate::ui) topology: Option<sot_protocol::topology::Topology>,
    pub(in crate::ui) settings: Settings,
    pub(in crate::ui) keybindings: KeyBindings,
    pub(in crate::ui) ledger: Arc<std::sync::Mutex<NativeProgressLedger>>,
}

#[cfg(all(test, feature = "test-window-progress"))]
thread_local! {
    static NATIVE_STARTUP: std::cell::RefCell<Option<NativeStartupInputs>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, feature = "test-window-progress"))]
pub(in crate::ui) fn with_native_startup<T>(
    inputs: NativeStartupInputs,
    body: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<NativeStartupInputs>);
    impl Drop for Restore {
        fn drop(&mut self) {
            NATIVE_STARTUP.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(NATIVE_STARTUP.with(|slot| slot.replace(Some(inputs))));
    body()
}

#[cfg(all(test, feature = "test-window-progress"))]
fn native_progress_ledger() -> Option<Arc<std::sync::Mutex<NativeProgressLedger>>> {
    NATIVE_STARTUP.with(|slot| slot.borrow().as_ref().map(|i| i.ledger.clone()))
}

fn create_window(
    event_loop: &ActiveEventLoop,
    cli: &crate::cli::Cli,
    persisted_geom: &crate::ui::persist::resume::GlobalState,
    init_w: f64,
    init_h: f64,
) -> Result<Arc<Window>> {
    // Cross-platform window icon, decoded at runtime from the logo PNG that is
    // embedded into the binary at compile time — no Windows .rc/winres
    // resource compiler, so the build needs no extra tooling and is identical
    // on Linux/macOS. On Windows `with_window_icon` sets only ICON_SMALL (the
    // title-bar / Alt-Tab small icon); the *taskbar* button uses ICON_BIG,
    // which winit exposes separately as `with_taskbar_icon` (set below) — so
    // both must be set or the taskbar falls back to the default exe icon. On
    // X11 the window icon populates _NET_WM_ICON. (The desktop *shortcut* icon
    // is a separate thing, set to logo.ico by install-shortcut.ps1; this is
    // the icon of the running window, which the shortcut never controls.)
    // Non-fatal on decode failure: we just launch without a custom icon.
    fn load_window_icon() -> Option<Icon> {
        const LOGO_PNG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png"));
        let rgba = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
        // 512² source → a tidy 256² icon; the OS rescales per surface. Area-
        // averaging `thumbnail` matches the ui/preview/image/png.rs downscale idiom.
        let icon = image::imageops::thumbnail(&rgba, 256, 256);
        let (w, h) = icon.dimensions();
        Icon::from_rgba(icon.into_raw(), w, h).ok()
    }
    let mut attrs = Window::default_attributes()
        .with_title("Ship of Tools")
        .with_active(true)
        .with_inner_size(LogicalSize::new(init_w, init_h));
    if let Some(icon) = load_window_icon() {
        // Windows taskbar buttons use ICON_BIG; `with_window_icon` sets only
        // ICON_SMALL. Set the taskbar icon explicitly (256×256 ceiling, which
        // matches the thumbnail above) via the Windows extension trait, or the
        // taskbar shows the default exe icon while the title bar shows our logo.
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_taskbar_icon(Some(icon.clone()));
        }
        attrs = attrs.with_window_icon(Some(icon));
    }
    if let (Some(x), Some(y)) = (persisted_geom.window_x, persisted_geom.window_y) {
        attrs = attrs.with_position(LogicalPosition::new(x, y));
    }
    // Resume fullscreen across launches — especially the ADR 0017
    // self-relaunch, so a rebuild doesn't drop the user out of FS.
    // `--start-fullscreen` forces it independently of persisted state
    // (harness runs can't rely on the box's saved geometry; the docs
    // shots are taken fullscreen on an ultrawide).
    if persisted_geom.fullscreen == Some(true) || cli.start_fullscreen {
        attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
    }
    let window = Arc::new(
        event_loop
            .create_window(attrs)
            .context("failed to create winit window")?,
    );
    // Focus is deferred to the first rendered frame (see
    // `focus_on_first_frame` / the RedrawRequested handler): a focus
    // request issued here, before the window is shown, is ignored by
    // most window managers. `.with_active(true)` above is the portable
    // hint we land active; the post-paint pass does the real attempt.
    Ok(window)
}

#[derive(Clone, Copy)]
pub(super) struct CellMetrics {
    pub(super) scale: f32,
    pub(super) cell_h: f32,
    pub(super) chrome_origin_x: f32,
    pub(super) chrome_origin_y: f32,
}

fn compute_cell_metrics(cli: &crate::cli::Cli, window: &Arc<Window>) -> CellMetrics {
    // Combined scale: OS DPR for HiDPI displays + user override.
    let scale = (cli.scale * window.scale_factor() as f32).max(0.5);
    tracing::info!(
        dpr = window.scale_factor(),
        cli_scale = cli.scale,
        effective_scale = scale,
        "metric scale resolved"
    );
    let cell_h = BASE_CELL_H * scale;
    let chrome_origin_x = BASE_CHROME_ORIGIN_X * scale;
    let chrome_origin_y = BASE_CHROME_ORIGIN_Y * scale;
    CellMetrics { scale, cell_h, chrome_origin_x, chrome_origin_y }
}

fn load_splash_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    quad_pipeline: &QuadPipeline,
) -> Option<Quad> {
    // Startup splash: the bundled "Ship of Tools" wordmark fills the preview
    // pane until the user navigates (kernel-driven previews replace it).
    // Linear-sampled so the logo scales smoothly.
    let preview_png = quad_from_png_bytes(
        &device,
        &queue,
        &quad_pipeline,
        LOGO_WORDMARK_PNG,
        crate::ui::render::quad::SamplerKind::Linear,
    )
    .map_err(|e| {
        tracing::warn!(error = %e, "startup wordmark decode failed");
        e
    })
    .ok();
    preview_png
}

struct PreviewContent {
    preview_svg: Option<Quad>,
    highlight_service: crate::ui::preview::markdown::highlight::HighlightService,
    preview_md: MarkdownPreview,
    md_rect_px: ScreenRect,
    concept_rect_px: ScreenRect,
}

fn build_preview_content(
    text: &mut TextLayer,
    config: &wgpu::SurfaceConfiguration,
    scale: f32,
) -> Result<PreviewContent> {
    // No startup math SVG preload. The unified preview pane shows
    // whatever the cursor drives via `preview.get`; the SAMPLE_MATH_SVG
    // was useful for the pre-quadrant 4-tile demo (acceptance #2) but
    // it dominates the cascade on plain file navigation now. SVG comes
    // back only when math.render fires for the cursored content.
    let preview_svg: Option<Quad> = None;

    // HighlightService — tree-sitter parser pool. Constructed once
    // (per-language `HighlightConfiguration` compile is moderately
    // expensive) and reused across every `MarkdownPreview::new` /
    // re-shape call.
    let highlight_service = crate::ui::preview::markdown::highlight::HighlightService::new()
        .context("failed to build HighlightService")?;

    // Initial markdown buffer with the full surface as a fallback rect;
    // the first redraw replaces md_rect_px with the actual pane rect from
    // ratatui's layout pass and re-shapes against it.
    let _bootstrap_token_cache: std::collections::HashMap<
        (String, u64),
        Vec<crate::net::transport::MarkdownToken>,
    > = std::collections::HashMap::new();
    let preview_md = MarkdownPreview::new(
        text.font_system_mut(),
        SAMPLE_MARKDOWN,
        config.width as f32,
        config.height as f32,
        scale,
        &MathMetricsMap::new(),
        &FigureMetricsMap::new(),
        &highlight_service,
        &_bootstrap_token_cache,
    );
    let md_rect_px = ScreenRect {
        x: 0.0,
        y: 0.0,
        w: config.width as f32,
        h: config.height as f32,
    };
    let concept_rect_px = md_rect_px; // same fallback until first layout
    Ok(PreviewContent { preview_svg, highlight_service, preview_md, md_rect_px, concept_rect_px })
}

fn apply_harness_flags(state: &mut State, cli: &crate::cli::Cli) {
    // `--demo-sessions a,b:working,c` (capture harness): seed the
    // workspace strip offline so the bottom session strip renders without
    // a live backend. Middle entry is made active so both left + right
    // neighbours show. A `:state` suffix on an entry also seeds
    // `workspace_states[slug] = (state, now)` so its work-state tone
    // renders; a bare slug carries no state (renders as before).
    // ADR 0042 L2a: the harness has no real host, so every demo entry
    // is seeded under one synthetic `"demo"` host — consistent with
    // `active_host`'s own offline default (`"offline"` when `conns` is
    // empty, which it always is for these harness flags).
    let demo_host: HostKey = "demo".to_string();
    if !cli.demo_sessions.is_empty() {
        state.workspace_slugs = cli
            .demo_sessions
            .iter()
            .map(|s| (demo_host.clone(), s.clone()))
            .collect();
        let now_rfc3339 = chrono::Utc::now().to_rfc3339();
        for (i, s) in cli.demo_sessions.iter().enumerate() {
            let key: WsKey = (demo_host.clone(), s.clone());
            state.workspace_labels.insert(key.clone(), s.clone());
            if let Some(Some(st)) = cli.demo_session_states.get(i) {
                state
                    .workspace_states
                    .insert(key.clone(), (st.clone(), now_rfc3339.clone()));
                // Mirror into prev so a later live transition off this
                // seeded state would flash, not first-appear.
                state.prev_workspace_states.insert(key, st.clone());
            }
        }
        let mid = cli.demo_sessions.len() / 2;
        state.active_workspace_id = cli.demo_sessions.get(mid).cloned();
        state.active_host = demo_host.clone();
    }
    // `--demo-flash a,c` (capture harness): stamp a fresh status-change
    // flash on the listed slugs at startup so a `--capture` shows the
    // flash near full brightness. We also rewrite `prev_workspace_states`
    // to a *different* synthetic state so the same slug reads as a real
    // transition under the live diff path (rather than first-appearance).
    for slug in &cli.demo_flash {
        let key: WsKey = (demo_host.clone(), slug.clone());
        state
            .flash_starts
            .insert(key.clone(), std::time::Instant::now());
        // A prior state distinct from whatever was seeded above makes the
        // transition look real; "idle" unless the current seed is idle.
        let prior = match state.workspace_states.get(&key) {
            Some((cur, _)) if cur == "idle" => "working",
            _ => "idle",
        };
        state.prev_workspace_states.insert(key, prior.to_string());
    }
    // `--start-monitor` (capture harness): the drawer was opened at
    // construction; queue the same subscribe + history prefill the
    // Ctrl+M arm sends. The unbounded req channel buffers until the
    // transport connects, so sending here is safe pre-hello.
    if cli.start_monitor {
        // ADR 0042 L2a: the drawer (Monitor content included) rides a
        // FIXED connection always — it never follows `active_host`
        // around as the user switches workspaces. Which connection is
        // `monitor_host` (2.1): the declared hub, so the fleet's record
        // is what's shown regardless of which host this box dialled.
        let monitor_host = state.monitor_host();
        let _ = state.send_to(
            &monitor_host,
            crate::net::transport::OutgoingReq::MonitorSubscribe,
        );
        let _ = state.send_to(
            &monitor_host,
            crate::net::transport::OutgoingReq::MonitorHistory {
                window_s: 300.0,
                points: 300,
                until: None,
                host: None,
            },
        );
        state.monitor_view.subscribed = true;
        state.monitor_dirty = true;
    }
}

fn apply_startup_font_scale(state: &mut State, cli: &crate::cli::Cli) {
    // Font scale at startup, highest wins (maintainer note, 2026-07-03):
    //   0. `--font-scale` — the harness pin: docs captures must render
    //      at one size on ANY box, over zoom/settings/tier alike;
    //   1. persisted per-host zoom (Ctrl+=/-/0 → state-<host>.toml) —
    //      the user's explicit choice ALWAYS wins and is never clobbered
    //      by a default (the seed path below never persists);
    //   2. `[font] scale` in settings.toml — an explicit machine opinion;
    //   3. built-in monitor-width tier — wide displays default larger
    //      ("default is a bit small" on a 4096px ultrawide);
    //   4. 1.0.
    // apply_text_scale propagates through cell metrics + text layer; it
    // does *not* persist, so none of this clobbers the saved nav cursor
    // before the tree reloads.
    //
    // Harness runs (--ephemeral / --capture) skip the PERSISTED restore —
    // their documented contract is "no per-host shared-state interaction",
    // and inheriting the box's local zoom made captures box-dependent
    // (found by the docs pipeline). They still get the settings/tier seed,
    // and the harness `--font-scale` flag (wt/docs) pins over everything.
    if let Some(fs) = cli.font_scale {
        state.apply_text_scale(fs);
    } else if let Some(fs) = (!state.ephemeral)
        .then(|| crate::ui::persist::resume::load().font_scale)
        .flatten()
    {
        if (fs - 1.0).abs() > 0.001 {
            state.apply_text_scale(fs as f32);
        }
    } else {
        let monitor_w = state
            .window
            .current_monitor()
            .map(|m| m.size().width)
            .unwrap_or(0);
        let seed = state
            .settings
            .font_scale
            .unwrap_or_else(|| default_font_scale_for_width(monitor_w));
        if (seed - 1.0).abs() > 0.001 {
            state.apply_text_scale(seed);
        }
    }
}
