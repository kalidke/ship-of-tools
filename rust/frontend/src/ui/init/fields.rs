//! `State::from_parts`: the struct literal that gives every `State` field its first value, and the
//! `initial_<field>` rules for the fields whose first value depends on the command line or the resume file.

use super::*;

pub(super) struct StartupParts {
    pub(super) evt_rx:
        std::sync::mpsc::Receiver<(crate::dial::HostKey, crate::transport::IncomingEvt)>,
    pub(super) conns: Vec<(crate::dial::HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)>,
    pub(super) leases: Arc<crate::lease::Leases>,
    pub(super) launch: LaunchInputs,
    pub(super) window: Arc<Window>,
    pub(super) metrics: CellMetrics,
    pub(super) gpu: GpuSurface,
    pub(super) text_grid: TextGrid,
    pub(super) solid: SolidQuads,
    pub(super) logos: LogoQuads,
    pub(super) preview_png: Option<Quad>,
    pub(super) content: PreviewContent,
    pub(super) repo_dir: Option<std::path::PathBuf>,
    pub(super) harness: bool,
    pub(super) want_terminal_init: bool,
}

impl State {
    pub(super) fn from_parts(
        event_loop: &ActiveEventLoop,
        cli: &crate::cli::Cli,
        parts: StartupParts,
    ) -> Self {
        let StartupParts {
            evt_rx, conns, leases, launch, window, metrics, gpu, text_grid, solid, logos,
            preview_png, content, repo_dir, harness, want_terminal_init,
        } = parts;
        let LaunchInputs {
            persisted_geom, active_host, resume_matches_last_host, monitor_hub, settings, ..
        } = launch;
        let CellMetrics { scale, cell_h, chrome_origin_x, chrome_origin_y } = metrics;
        let GpuSurface { surface, device, queue, config, surface_format } = gpu;
        let TextGrid { text, cell_w, terminal } = text_grid;
        let SolidQuads {
            quad_pipeline, selection_bg_quad, overlay_back_quad, code_bg_quad, code_border_quad,
            strike_line_quad, scalebar_bar_quad, scalebar_back_quad, caption_back_quad,
        } = solid;
        let LogoQuads { logo_quad, wordmark_quad } = logos;
        let PreviewContent {
            preview_svg, highlight_service, preview_md, md_rect_px, concept_rect_px,
        } = content;
        Self {
            window,
            surface,
            device,
            queue,
            config,
            text,
            background: initial_background(surface_format),
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
            bl_pane_target:
                initial_bl_pane_target(resume_matches_last_host, &persisted_geom, &active_host),
            active_workspace_id:
                initial_active_workspace_id(cli, resume_matches_last_host, &persisted_geom),
            hosts: crate::net::hosts::HostTable {
                host_connected: HashMap::new(),
                // Filled by `resumed()` from the same `PendingTransport` list
                // `conns` came from, before that list is consumed spawning
                // each host's transport task — empty here only briefly.
                host_transports: HashMap::new(),
                host_resolved_dial: HashMap::new(),
                link_gates: HashMap::new(),
                declared_host: HashMap::new(),
                reconnect_now: Arc::new(tokio::sync::Notify::new()),
            },
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
            pending_resume_nav: initial_pending_resume_nav(cli),
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
            leases,
            #[cfg(windows)]
            own_state_root: crate::paths::sot_state_dir().map(|d| sot_log::state_dir::state_dir_hash(&d)),
            not_ended_shown: None,
            leaving: None,
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
            focus: initial_focus(cli),
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
            monitor_aspect: initial_monitor_aspect(event_loop),
            drawer: initial_drawer(cli, want_terminal_init),
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
        }
    }
}

fn initial_background(surface_format: wgpu::TextureFormat) -> wgpu::Color {
    clear_color_for_surface(
        // Deep midnight navy — clearly blue (not the previous
        // neutral near-black) while staying dim enough that
        // foreground glyphs and the yellow selection rect read
        // unambiguously on top.
        (0.020, 0.035, 0.090),
        surface_format.is_srgb(),
    )
}

fn initial_bl_pane_target(
    resume_matches_last_host: bool,
    persisted_geom: &crate::state_persistence::GlobalState,
    active_host: &HostKey,
) -> Option<(HostKey, String)> {
    // Restored BL target so the first pty.open re-attaches to
    // wherever the last session left off. None → DEFAULT (sot-llm).
    // Only restored when we actually resumed onto the host it was
    // saved for (`resume_matches_last_host`, ADR 0042 L2a codex
    // review item H) -- a session name saved for a DIFFERENT host
    // (G's fallback kicked in) could collide with an unrelated
    // same-named session on this one.
    if resume_matches_last_host {
        persisted_geom
            .last_bl_target
            .clone()
            .map(|t| (active_host.clone(), t))
    } else {
        None
    }
}

fn initial_active_workspace_id(
    cli: &crate::cli::Cli,
    resume_matches_last_host: bool,
    persisted_geom: &crate::state_persistence::GlobalState,
) -> Option<String> {
    // Harness runs (capture or --ephemeral) skip the restore: a
    // persisted workspace switch re-fires tree.root + a root preview
    // after --capture-preview's one-shot, clobbering the captured
    // node with whatever the live session was parked on. Harness
    // runs must be deterministic. Also gated on
    // `resume_matches_last_host` -- see bl_pane_target above.
    if cli.capture.is_some()
        || cli.ephemeral
        || !resume_matches_last_host
    {
        None
    } else {
        persisted_geom.last_workspace_id.clone()
    }
}

fn initial_pending_resume_nav(cli: &crate::cli::Cli) -> Option<(String, u16)> {
    if cli.capture.is_some() || cli.ephemeral {
        // Same determinism rule as active_workspace_id above.
        None
    } else {
        let p = crate::state_persistence::load();
        p.nav_selected_id.map(|id| (id, p.nav_scroll.unwrap_or(0)))
    }
}

fn initial_focus(cli: &crate::cli::Cli) -> PaneFocus {
    match cli.start_focus.as_str() {
        "preview" => PaneFocus::Preview,
        "llm" => PaneFocus::Llm,
        "repl" => PaneFocus::Repl,
        _ => PaneFocus::NavTree,
    }
}

fn initial_monitor_aspect(event_loop: &ActiveEventLoop) -> f32 {
    // Aspect of the primary monitor (or 1.6 = 16:10 fallback
    // when no monitor handle is available — headless capture
    // doesn't have one). Locked for the session per the user's
    // "no in-session reflow" preference.
    event_loop
        .primary_monitor()
        .map(|m| {
            let s = m.size();
            if s.height > 0 {
                s.width as f32 / s.height as f32
            } else {
                1.6
            }
        })
        .unwrap_or(1.6)
}

fn initial_drawer(cli: &crate::cli::Cli, want_terminal_init: bool) -> DrawerContent {
    // Open straight into the Terminal drawer on a self-relaunch —
    // see `want_terminal_init` above.
    if cli.start_monitor {
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
    }
}
