//! `State::new`: the launch sequence that builds the window's state, one step per function, in order.

use super::*;

impl State {
    pub(super) fn new(
        event_loop: &ActiveEventLoop,
        evt_rx: std::sync::mpsc::Receiver<(crate::dial::HostKey, crate::transport::IncomingEvt)>,
        cli: &crate::cli::Cli,
        conns: Vec<(
            crate::dial::HostKey,
            tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
        )>,
        leases: Arc<crate::lease::Leases>,
    ) -> Result<Self> {
        // Loaded here (rather than at each of its several uses below) so
        // `last_host` and the window-geometry fields below all read the
        // SAME snapshot of the file.
        let persisted_geom = crate::state_persistence::load();
        // ADR 0042 L2a codex review, item H: `last_host` is the active
        // host AT QUIT (persist_resume_state writes it every save now —
        // see the field's own doc for the ADR 0015 -> L2a meaning
        // change). It wins whenever it's still a resolved connection, so a
        // daily launch resumes wherever the user actually left off;
        // `resolve_default_host` (G's rule — no more configured
        // `default_host` since topology plan lane D, so this always falls
        // back to `conns.first()`) is the fallback when there's no
        // persisted host, or it's no longer reachable.
        let active_host: crate::dial::HostKey = persisted_geom
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
            // averaging `thumbnail` matches the preview/png.rs downscale idiom.
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

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .context("failed to create wgpu surface")?;

        // We draw glyph quads and image blits — a 2D workload an integrated
        // GPU handles fine — so the default is LowPower. Asking for the
        // discrete adapter on a hybrid-graphics laptop keeps it awake for the
        // whole session (~11 W measured on an idle RTX 4070, 2026-07-31); an
        // awake dGPU cannot power-gate. `[gpu] power_preference = "high"`
        // opts back in for desktops with a real GPU. No-op on single-adapter
        // machines, where HighPerformance already resolved to the iGPU.
        // Binds once, here — changing the key needs an FE restart.
        let power_preference = match settings.gpu_power_preference {
            crate::settings::GpuPowerPreference::Low => wgpu::PowerPreference::LowPower,
            crate::settings::GpuPowerPreference::High => wgpu::PowerPreference::HighPerformance,
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("no compatible wgpu adapter found")?;
        tracing::info!(
            requested = ?settings.gpu_power_preference,
            adapter = %adapter.get_info().name,
            device_type = ?adapter.get_info().device_type,
            backend = ?adapter.get_info().backend,
            "wgpu adapter selected"
        );

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("sot-device"),
                required_features: wgpu::Features::empty(),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .context("failed to request wgpu device")?;

        let size = window.inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            // COPY_SRC enables the `--capture` readback path. Cheap when
            // unused; widely supported on the wgpu backends we care about
            // (DX12/Vulkan/Metal).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            // AutoVsync, NOT `present_modes[0]`: the capability list's order
            // is driver-specific, so [0] picked a non-vsync mode (Immediate/
            // Mailbox) on some GPUs — visible flicker/tearing since day one on
            // those machines, worst in fullscreen where DWM composition stops
            // masking it (a Windows FE box finding, 2026-07-12; the same build was clean
            // on hardware whose driver lists Fifo first). AutoVsync =
            // FifoRelaxed where supported, else Fifo — vsynced on every
            // backend.
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            // One queued frame fewer between input and photon.
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        let mut text = TextLayer::new(&device, &queue, surface_format, scale);
        text.resize(&queue, config.width, config.height);

        // Derive cell_w from the actual monospace glyph advance instead
        // of BASE_CELL_W = 9.0 — the static constant didn't match cosmic-
        // text's real advance (~7.7px for Consolas at 14pt), and the gap
        // grew with column count, making the cursor visibly outpace the
        // typed text in REPL / LLM panes. Fall back to the constant on
        // shape failure so a missing monospace font doesn't kill startup.
        let measured = text.monospace_advance();
        let cell_w = measured.unwrap_or(BASE_CELL_W * scale);
        tracing::info!(
            measured = measured.unwrap_or(0.0),
            cell_w,
            "monospace advance measured"
        );

        let (cols, rows) = cell_grid_for(
            config.width,
            config.height,
            cell_w,
            cell_h,
            chrome_origin_x,
            chrome_origin_y,
        );
        let backend = WgpuBackend::new(cols, rows);
        let terminal = Terminal::new(backend)
            .context("failed to construct ratatui Terminal over WgpuBackend")?;

        let quad_pipeline = QuadPipeline::new(&device, surface_format);
        // 1×1 translucent yellow texture for the LLM-pane selection
        // highlight. Alpha 140 (~55%) lets the chrome's default-fg light
        // text on the dark bg remain legible through the tint — opaque
        // yellow would either bleach the (204,204,204) fg into a yellow-
        // green blur or force a fg-colour switch that the chrome
        // pipeline doesn't currently carry through selection state.
        let selection_bg_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[252, 240, 130, 140], 1, 1)
                .context("failed to build selection_bg_quad")?;
        // Nav-spill overlay backing: the surface's deep-navy tone
        // ((0.020, 0.035, 0.090) visible-srgb ≈ (5, 9, 23) u8) at
        // NAV_SPILL_BACK_ALPHA so the strip reads as the nav background
        // continuing over the preview, with imagery barely ghosting
        // through. Alpha is a tune-by-eye knob.
        let overlay_back_quad = Quad::from_rgba8(
            &device,
            &queue,
            &quad_pipeline,
            &[5, 9, 23, NAV_SPILL_BACK_ALPHA],
            1,
            1,
        )
        .context("failed to build overlay_back_quad")?;
        // Markdown code-bg panel. (52, 60, 92, 230) is VS-Code Dark+'s
        // `#1e1e1e`-leaning panel tone lifted a touch toward the
        // chrome's midnight-navy surface so the panel reads as "lifted
        // off the page" without looking glued to the deep-navy bg.
        // Slight alpha (230/255) softens the edge against the antialias
        // halo of surrounding non-code glyphs.
        let code_bg_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[52, 60, 92, 230], 1, 1)
                .context("failed to build code_bg_quad")?;
        // Code-block border — one step lighter than the bg quad so the
        // outline reads against the panel's slate fill. Full alpha
        // because a soft border just looks fuzzy at 1 px width.
        let code_border_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[88, 100, 140, 255], 1, 1)
                .context("failed to build code_border_quad")?;
        // Strikethrough line — matches the default-fg tone so the
        // line reads "the same colour as the text it's crossing
        // through" without per-glyph colour lookups. Alpha is full so
        // the line stands out crisply against the navy bg.
        let strike_line_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[204, 204, 204, 255], 1, 1)
                .context("failed to build strike_line_quad")?;
        // ADR 0034 scalebar: an opaque white bar over a translucent-black
        // backing box so the overlay reads on light OR dark rasters.
        let scalebar_bar_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[255, 255, 255, 255], 1, 1)
                .context("failed to build scalebar_bar_quad")?;
        let scalebar_back_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[0, 0, 0, 170], 1, 1)
                .context("failed to build scalebar_back_quad")?;
        // Caption backing: the same translucent black, a touch more opaque —
        // it sits under prose (which needs more contrast to stay readable than
        // a solid white bar does) and spans the pane width.
        let caption_back_quad =
            Quad::from_rgba8(&device, &queue, &quad_pipeline, &[0, 0, 0, 200], 1, 1)
                .context("failed to build caption_back_quad")?;

        // Decode the two embedded brand logos into textured quads. Both are
        // purely cosmetic chrome (strip badge flankers + nav wordmark), so a
        // decode/upload failure must NOT abort frontend startup — log a warning
        // and leave the field None; the affected draw is then simply skipped.
        // `quad_and_dims_from_bytes` builds a Linear-sampled quad (smooth
        // downscale) and returns the native (w, h) for aspect-ratio sizing.
        let logo_quad = match crate::preview::png::quad_and_dims_from_bytes(
            &device,
            &queue,
            &quad_pipeline,
            LOGO_DARK_PNG,
        ) {
            Ok((q, w, h)) => Some((q, w, h)),
            Err(e) => {
                tracing::warn!(error = %e, "failed to decode logo-dark.png; strip logos disabled");
                None
            }
        };
        let wordmark_quad = match crate::preview::png::quad_and_dims_from_bytes(
            &device,
            &queue,
            &quad_pipeline,
            LOGO_WORDMARK_PNG,
        ) {
            Ok((q, w, h)) => Some((q, w, h)),
            Err(e) => {
                tracing::warn!(error = %e, "failed to decode logo-wordmark-dark.png; nav wordmark disabled");
                None
            }
        };

        // Spike-step-4 placeholders. Kernel-driven previews replace both once
        // transport.rs is wired.
        // Probe order: exe-relative first so dropping `sample.png` next to
        // the binary works out of the box; then a repo-relative path
        // (`examples/preview/sample.png`) so a clean clone has content; then
        // cwd; then the legacy `/tmp` paths from Linux-side dev so existing
        // setups don't regress. Empty slot is fine — kernel-driven previews
        // replace this path once the wire carries PNG mime types.
        let mut probe_paths: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                probe_paths.push(dir.join("sample.png"));
                probe_paths.push(dir.join("../sample.png"));
                // From <repo>/rust/target/release/, ../../examples/preview
                // resolves to <repo>/examples/preview.
                probe_paths.push(dir.join("../../examples/preview/sample.png"));
            }
        }
        probe_paths.push(std::path::PathBuf::from("examples/preview/sample.png"));
        probe_paths.push(std::path::PathBuf::from("sample.png"));
        probe_paths.push(std::path::PathBuf::from("/tmp/heatmap_test.png"));
        probe_paths.push(std::path::PathBuf::from("/tmp/LossPlot_v2g.png"));
        // Startup splash: the bundled "Ship of Tools" wordmark fills the preview
        // pane until the user navigates (kernel-driven previews replace it).
        // Linear-sampled so the logo scales smoothly. Falls back to a probed
        // sample.png only if the bundled wordmark ever fails to decode.
        let preview_png = quad_from_png_bytes(
            &device,
            &queue,
            &quad_pipeline,
            LOGO_WORDMARK_PNG,
            crate::preview::quad::SamplerKind::Linear,
        )
        .map_err(|e| {
            tracing::warn!(error = %e, "startup wordmark decode failed; probing sample.png");
            e
        })
        .ok()
        .or_else(|| {
            probe_paths
                .iter()
                .find_map(|p| std::fs::read(p).ok().map(|b| (p, b)))
                .and_then(|(path, bytes)| {
                    tracing::info!(path = %path.display(), "sample PNG loaded");
                    quad_from_png_bytes(
                        &device,
                        &queue,
                        &quad_pipeline,
                        &bytes,
                        crate::preview::quad::SamplerKind::Nearest,
                    )
                    .ok()
                })
        });

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
        let highlight_service = crate::preview::highlight::HighlightService::new()
            .context("failed to build HighlightService")?;

        // Initial markdown buffer with the full surface as a fallback rect;
        // the first redraw replaces md_rect_px with the actual pane rect from
        // ratatui's layout pass and re-shapes against it.
        let _bootstrap_token_cache: std::collections::HashMap<
            (String, u64),
            Vec<crate::transport::MarkdownToken>,
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


        let mut state = Self {
            window,
            surface,
            device,
            queue,
            config,
            text,
            background: clear_color_for_surface(
                // Deep midnight navy — clearly blue (not the previous
                // neutral near-black) while staying dim enough that
                // foreground glyphs and the yellow selection rect read
                // unambiguously on top.
                (0.020, 0.035, 0.090),
                surface_format.is_srgb(),
            ),
            terminal,
            quad_pipeline,
            selection_bg_quad,
            overlay_back_quad,
            code_bg_quad,
            code_border_quad,
            strike_line_quad,
            scalebar_bar_quad,
            scalebar_back_quad,
            caption_back_quad,
            border_quads: HashMap::new(),
            logo_quad,
            wordmark_quad,
            preview_png,
            preview_png_zoom: 1.0,
            preview_png_pan_px: (0.0, 0.0),
            preview_png_dims: None,
            preview_png_src_dims: None,
            preview_roi: None,
            pending_roi_capture_host: None,
            pending_roi_aim: None,
            preview_png_cache: HashMap::new(),
            pending_roi_restore: None,
            preview_svg,
            highlight_service,
            preview_md,
            md_rect_px,
            monitor_view: crate::monitor_view::MonitorView::new(),
            monitor_quad: None,
            monitor_rect_px: ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            monitor_dirty: false,
            repl_images: std::collections::HashMap::new(),
            repl_image_slots: Vec::new(),
            repl_scrollback_px: ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            repl_window: (0, 0),
            repl_build_anchor: None,
            evt_rx,
            status: "offline · no transport".to_string(),
            nav_prompt: None,
            pending_created_node_id: None,
            pending_deleted_node_id: None,
            notify_sticky_until: None,
            tree: TreeView::new(),
            mode: initial_mode(
                cli.start_mode.as_deref(),
                persisted_geom.last_mode.as_deref(),
                harness,
            ),
            concept_target_fired: None,
            last_cursor_pos: None,
            cursor_moved_at: None,
            concept: None,
            pending_file_edit: None,
            preview_node_id_fired: None,
            driven_preview_hold_cursor: None,
            pending_reveal: None,
            restore_nav_after_resume: None,
            reveal_awaiting: None,
            reveal_refetched: None,
            preview_page: None,
            preview_page_raster_zoom: 1.0,
            preview_page_raster_pending: None,
            preview_reraster_keep_view: false,
            preview_scale: None,
            scale_entry_prior_focus: None,
            scale_save_pending: None,
            // Default off; `--start-scalebar` arms it for the headless capture
            // harness (no `b` keypress). Still gated on a present scale at draw.
            scalebar_on: cli.start_scalebar,
            scalebar_label: None,
            preview_captions: CaptionStore::default(),
            caption_label: None,
            caption_band_px: 0.0,
            preview_anchor_line: None,
            preview_anchored_to: None,
            pinned_preview_node_id: None,
            // Restored BL target so the first pty.open re-attaches to
            // wherever the last session left off. None → DEFAULT (sot-llm).
            // Only restored when we actually resumed onto the host it was
            // saved for (`resume_matches_last_host`, ADR 0042 L2a codex
            // review item H) -- a session name saved for a DIFFERENT host
            // (G's fallback kicked in) could collide with an unrelated
            // same-named session on this one.
            bl_pane_target: if resume_matches_last_host {
                persisted_geom
                    .last_bl_target
                    .clone()
                    .map(|t| (active_host.clone(), t))
            } else {
                None
            },
            // Harness runs (capture or --ephemeral) skip the restore: a
            // persisted workspace switch re-fires tree.root + a root preview
            // after --capture-preview's one-shot, clobbering the captured
            // node with whatever the live session was parked on. Harness
            // runs must be deterministic. Also gated on
            // `resume_matches_last_host` -- see bl_pane_target above.
            active_workspace_id: if cli.capture.is_some()
                || cli.ephemeral
                || !resume_matches_last_host
            {
                None
            } else {
                persisted_geom.last_workspace_id.clone()
            },
            host_connected: HashMap::new(),
            workspace_lists: HashMap::new(),
            pending_destroy_target: None,
            host: None,
            daemon_root_basename: None,
            daemon_project_root: None,
            backend_version: None,
            scan_project_root: None,
            last_revision: 0,
            workspace_labels: HashMap::new(),
            workspace_project_roots: HashMap::new(),
            workspace_states: HashMap::new(),
            repl_lifecycle: HashMap::new(),
            workspace_id_slugs: HashMap::new(),
            pending_nav: HashMap::new(),
            prev_workspace_states: HashMap::new(),
            flash_starts: HashMap::new(),
            contrast_dim: cli.contrast_mode == "dim",
            workspace_slugs: Vec::new(),
            default_workspace_slug: None,
            workspace_picker: None,
            workspace_ui_snapshots: HashMap::new(),
            workspace_repl_snapshots: HashMap::new(),
            eval_id_workspace: HashMap::new(),
            repl_runfile_status: HashMap::new(),
            preview_concept: None,
            concept_rect_px,
            file_ast_hashes: std::collections::HashMap::new(),
            file_parse_fired: std::collections::HashSet::new(),
            pending_initial_selection: cli.start_selected,
            pending_resume_nav: if cli.capture.is_some() || cli.ephemeral {
                // Same determinism rule as active_workspace_id above.
                None
            } else {
                let p = crate::state_persistence::load();
                p.nav_selected_id.map(|id| (id, p.nav_scroll.unwrap_or(0)))
            },
            nav_readme_defaulted: std::collections::HashSet::new(),
            pending_switch_reveal: None,
            tree_store: TreeStore::new(),
            pending_auto_expand: cli.auto_expand,
            pending_auto_pin: cli.auto_pin,
            pending_demo_function_methods: cli.demo_function_methods.clone(),
            pending_demo_repl_eval: cli.demo_repl_eval.clone(),
            file_parse_retry: std::collections::HashMap::new(),
            pending_start_path: cli.start_path.clone(),
            start_path_fired: None,
            conns,
            // Filled by `resumed()` from the same `PendingTransport` list
            // `conns` came from, before that list is consumed spawning
            // each host's transport task — empty here only briefly.
            host_transports: HashMap::new(),
            leases,
            #[cfg(windows)]
            own_state_root: crate::paths::sot_state_dir().map(|d| sot_log::state_dir::state_dir_hash(&d)),
            not_ended_shown: None,
            leaving: None,
            host_resolved_dial: HashMap::new(),
            link_gates: HashMap::new(),
            declared_host: HashMap::new(),
            last_declared_sessions: None,
            active_host,
            monitor_hub,
            scale,
            cell_w,
            cell_h,
            chrome_origin_x,
            chrome_origin_y,
            capture_path: cli.capture.clone(),
            selfie_pending: None,
            capture_preview: cli.capture_preview.clone(),
            capture_preview_armed: cli.capture_preview.is_some(),
            capture_delay_ms: cli.capture_delay_ms,
            capture_cycle: cli.capture_cycle,
            ephemeral: cli.ephemeral || cli.capture.is_some(),
            frame_counter: 0,
            should_exit: false,
            last_key: None,
            battery_label: None,
            last_battery_query: None,
            dirty: false,
            last_frame_at: None,
            strip_scroll_px: None,
            strip_anim_last: None,
            wheel_angle: 0.0,
            wheel_vel: 0.0,
            wheel_anim_last: None,
            focus: match cli.start_focus.as_str() {
                "preview" => PaneFocus::Preview,
                "llm" => PaneFocus::Llm,
                "repl" => PaneFocus::Repl,
                _ => PaneFocus::NavTree,
            },
            repl_log: Vec::new(),
            repl_input: String::new(),
            repl_eval_counter: 0,
            tree_scroll: 0,
            pty_size: None,
            repl_scroll: 0,
            preview_scroll: 0,
            md_table_scroll_px: 0.0,
            table_buffers: Vec::new(),
            pane_rects: PaneRects::default(),
            llm_selection: None,
            llm_drag_active: false,
            cursor_px: (0.0, 0.0),
            wheel_residue_y: 0.0,
            maximized: cli.start_maximized,
            wide_preview: false,
            nav_spill_until: None,
            nav_spill_segments: Vec::new(),
            nav_spill_cursor: None,
            bindings: KeyBindings::load_layered(),
            settings,
            upload: None,
            upload_batch: None,
            // Aspect of the primary monitor (or 1.6 = 16:10 fallback
            // when no monitor handle is available — headless capture
            // doesn't have one). Locked for the session per the user's
            // "no in-session reflow" preference.
            monitor_aspect: event_loop
                .primary_monitor()
                .map(|m| {
                    let s = m.size();
                    if s.height > 0 {
                        s.width as f32 / s.height as f32
                    } else {
                        1.6
                    }
                })
                .unwrap_or(1.6),
            // Open straight into the Terminal drawer on a self-relaunch —
            // see `want_terminal_init` above.
            drawer: if cli.start_monitor {
                // `--start-monitor` (capture harness) wins over the
                // relaunch terminal default: an explicit capture ask beats
                // the relaunch convenience. The subscribe + history prefill
                // for this startup-opened drawer is sent right after
                // construction, where `req_tx` is wired up.
                DrawerContent::Monitor
            } else if want_terminal_init {
                DrawerContent::Terminal
            } else {
                DrawerContent::Closed
            },
            local_term: None,
            #[cfg(windows)]
            attach_term: None,
            pane_attach_term: None,
            warm_attach: WarmAttachPool::new(),
            pane_hold: None,
            pane_dial_error: None,
            read_mark: None,
            pane_attach_requested_at: None,
            pending_capsule_create_requested_at: None,
            pane_attach_started_at: None,
            pane_attach_episode_warnings: 0,
            pane_attach_presented: false,
            pane_feed: PaneFeed::Pending,
            pane_inputs_discarded: 0,
            term_size: None,
            repo_dir,
            relaunch_flag: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            presence_last_sent: None,
            fe_commands: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            fe_state_sig: None,
            focus_on_first_frame: true,
            edit_state: None,
            preview_edit: None,
            help: help::Help::default(),
            help_origin: None,
            help_start_pending: cli.start_help,
            help_peek_start_pending: cli.start_help_peek,
            help_back_quad: None,
            protocol_mismatch: HashMap::new(),
            preview_fatal: None,
            math_cache: std::collections::HashMap::new(),
            math_pending: std::collections::HashSet::new(),
            markdown_token_cache: std::collections::HashMap::new(),
            markdown_token_pending: std::collections::HashSet::new(),
            figure_cache: std::collections::HashMap::new(),
            figure_pending: std::collections::HashSet::new(),
            figure_failed: std::collections::HashSet::new(),
            current_md_node_id: None,
            current_md_workspace_id: None,
            needs_md_reflow: false,
            reconnect_now: Arc::new(tokio::sync::Notify::new()),
            // ADR 0035: each host's `Connected` evt (remote + proxy) adds it
            // here; `resumed()` spawns the listener manager.
            proxy_capable_hosts: std::collections::HashSet::new(),
            proxy_listener_tx: None,
            proxy_ensured: std::collections::HashMap::new(),
            repl_pkg_mode: false,
            history_pos: None,
            history_saved: None,
            text_scale_mult: 1.0,
            preview_src: None,
            preview_src_node_id: None,
            preview_req_gen: 0,
            concept_req_gen: 0,
            project_scan_req_gen: HashMap::new(),
        };
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
                crate::transport::OutgoingReq::MonitorSubscribe,
            );
            let _ = state.send_to(
                &monitor_host,
                crate::transport::OutgoingReq::MonitorHistory {
                    window_s: 300.0,
                    points: 300,
                    until: None,
                    host: None,
                },
            );
            state.monitor_view.subscribed = true;
            state.monitor_dirty = true;
        }
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
            .then(|| crate::state_persistence::load().font_scale)
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
        Ok(state)
    }
}
