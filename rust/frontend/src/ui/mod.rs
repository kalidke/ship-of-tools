//! The window's state: `State`, the one struct every folder under ui/ reads and writes, and the
//! module root that declares those folders.

pub(crate) mod input;
use input::*;
pub(crate) mod persist;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Icon, Window, WindowId};

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line as RtLine, Span},
    widgets::Paragraph,
    Terminal,
};

use crate::ui::render::cells::WgpuBackend;
use crate::ui::preview::editor::buffer::EditBuffer;
use crate::net::dial::HostKey;
use crate::ui::input::keybindings::{Action, KeyBindings, Modifiers};
use crate::ui::input::help;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use crate::ui::preview::markdown::{
    FigureMetrics, FigureMetricsMap, MarkdownPreview, MathMetrics, MathMetricsMap,
    BODY_SIZE as MD_BODY_SIZE,
};
use crate::ui::preview::image::png::quad_from_png_bytes;
use crate::ui::render::quad::{Quad, QuadPipeline, ScreenRect};
use crate::ui::preview::image::svg::quad_from_svg_bytes;
use crate::ui::preview::markdown::media::{
    parse_math_svg_dims, whole_row_bottom, MathSvg, TableBufferEntry, MATHJAX_EX_FACTOR,
};
use crate::ui::persist::settings::Settings;
use crate::net::transport::OutgoingReq;
use crate::net::hosts::{PendingTransport, lane_dial, resolve_default_host, resolve_monitor_host};
use crate::pages::open_html_in_browser;
use crate::lease::{ExitReason, ExitStep, close_now, exit_intent};
use crate::relaunch::relaunch_sentinel_path;
#[cfg(windows)]
use crate::relaunch::{allow_next_foreground, force_os_foreground};
use crate::net::identity::self_comm_handle;
use crate::net::identity::frontend_identity;
use sot_protocol::ops::LeaveIntent;
use sot_protocol::{ReplFrame, TreeNode};

mod app;
use app::*;
pub(crate) use app::{App, MainWake};
use preview::image::figures::{decode_figure_bytes, fail_figure, FigureCacheEntry};
#[cfg(test)]
use preview::image::overlay::CAPTION_MAX_CHARS;
use preview::image::overlay::{
    parse_physical_scale, truncate_caption, CaptionStore,
};
use preview::image::view::{
    image_rect_for_caption, letterbox, png_cache_key_from_node_id, png_zoom_max,
    preview_image_pane_px, roi_rects_within_quantization, solve_roi_view, visible_roi_px,
    PreviewRoi, RoiAim, RoiRect,
};
mod nav;
use nav::*;

pub(crate) mod chrome;
use chrome::*;
pub(crate) mod drawer;
use drawer::repl::presentation::ReplPresentation;
use drawer::repl::log::ReplEntry;
use drawer::repl::{DraftEdit, ReplModel, ReplTarget};
use drawer::terminal::pump::scroll_drawer_ring;
use drawer::terminal::vt::{key_to_pty_bytes, paint_terminal, scroll_ring};

mod agent_pane;
use agent_pane::*;

mod control;
use control::*;

mod session;
use session::*;

/// `(host, identifier)` — the composite identity backing every
/// workspace-scoped cache once workspaces come from more than one daemon
/// connection (ADR 0042 L2a). `identifier` is a slug, a tmux session name,
/// or a workspace_id depending on the field — each field's own doc names
/// which; the shape is reused rather than adding a distinct wrapper per
/// meaning, since every one of them exists to answer the same question
/// ("whose is this, and which one on that host"). A bare identifier is
/// ambiguous the moment two hosts each have one with the same name.
type WsKey = (HostKey, String);

/// Sessions-tree host-node divider (ADR 0042 L2a, "the wheel icon between
/// hosts"). No settings/wheel glyph already ships anywhere in the FE's tree
/// or chrome rendering (surveyed: only `●` — semantically taken, the ADR
/// 0025 pending-result badge — and box-drawing pane-border glyphs exist);
/// this is a judgment call under "use the glyph the FE already ships for
/// something else", argued in the L2a report rather than silently assumed.
/// A single well-covered Unicode symbol, not a bundled font.
const HOST_DIVIDER_GLYPH: &str = "⚙";































use crate::ui::render::text::TextLayer;












// ADR 0045 decision 1 (Codex review, lane B5 discharge); reshaped by C3 as
// amended (isolation-plan.md §3, dev/output/c3-second-connection-
// amendment.md §1): `ResolvedDial` — which transport a host's CONTROL
// connection actually resolved to — moved to `crate::net::transport`, beside
// `TransportConfig`, because `IncomingEvt::Connected` now carries it. See
// its doc there. No longer `Copy` (`SshRecipe` isn't); every former
// `.copied()` reader below is `.cloned()`.
use crate::net::transport::ResolvedDial;

mod connections;
mod page_proxy;
mod events;
mod init;



struct State {
    // ui/render: the wgpu surface and device, text layer, fixed quads, cell metrics and --capture state.
    window: MainWindow,
    /// What another thread holds to ask this window for a redraw.
    wake: MainWake,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    text: TextLayer,
    background: wgpu::Color,
    terminal: Terminal<WgpuBackend>,
    quad_pipeline: QuadPipeline,
    /// A 1×1 pastel-yellow RGBA texture used as the LLM-pane selection
    /// highlight. Rendered as a per-row stretched quad before text.render
    /// so glyphs sit on top — gives a "highlighter" look that doesn't
    /// invert the text colour (REVERSED inverts both fg and bg, which
    /// hurt readability per user feedback). Solid colour with full alpha
    /// — the chrome's near-black bg shows around glyph antialias edges.
    selection_bg_quad: Quad,
    /// A 1×1 near-opaque surface-navy RGBA texture: the backing strip
    /// under nav-spill overlay rows. Rendered AFTER the main text pass
    /// (so it covers preview glyphs and images alike), then the overlay
    /// text layer draws the row text on top. Alpha slightly under full
    /// so an image preview barely ghosts through — reads as "floating
    /// above", not "hole cut into the preview".
    overlay_back_quad: Quad,
    /// A 1×1 dark slate RGBA texture used as the markdown-preview code
    /// bg — both inline `<code>` and fenced code blocks render against
    /// it so `Vec<u8>` doesn't look like prose. Paint pass walks
    /// `MarkdownPreview::code_glyph_rects` and stretches this quad
    /// behind each contiguous code-run, slightly lifted from the
    /// chrome bg toward VS-Code Dark+'s `#1e1e1e` so the panel reads
    /// clearly against the deep-navy surface.
    code_bg_quad: Quad,
    /// 1×1 lighter-slate RGBA tile for the 1-px border around fenced
    /// code blocks. Four thin quads per block (top/bottom/left/right)
    /// give the panel an outline so it reads as a discrete block
    /// instead of "a stripe of slate behind the text" — matches the
    /// GitHub / VS Code code-block treatment.
    code_border_quad: Quad,
    /// 1×1 light-gray RGBA tile painted as a thin horizontal quad
    /// through every `STRIKE_GLYPH_FLAG` run in the markdown preview.
    /// Replaces the U+0335 / U+0336 combining-mark fallback that
    /// rasterised inconsistently across fontdb's font picks (Segoe UI
    /// rendered it as an underline, Consolas barely at all).
    strike_line_quad: Quad,
    /// Lazily-built solid-colour quads for pane-border box-drawing chars,
    /// keyed by RGB. `chrome::project_border_quads` emits arm-from-centre
    /// rects sized to the exact cell so stacked `│` borders tile without the
    /// sub-cell font-leading gaps cosmic-text leaves (the resolved monospace
    /// font doesn't fill the 18px cell). Cached rather than built per frame
    /// because `Quad::from_rgba8` allocates a GPU texture + bind group; pane
    /// borders use only 1–2 colours so the map stays tiny.
    border_quads: HashMap<(u8, u8, u8), Quad>,
    /// Solid quads for the ADR-0034 scalebar overlay: a white `bar` over a
    /// black `back`ing box. Dedicated fields (not `border_quads`) because each
    /// `render_many` mutably borrows its quad for the whole render-pass
    /// lifetime, so the two colours must live in disjoint fields.
    scalebar_bar_quad: Quad,
    scalebar_back_quad: Quad,
    /// Backing box for the figure-caption overlay. A dedicated field, not a
    /// reuse of `scalebar_back_quad`: `render_many` mutably borrows its quad for
    /// the whole render-pass lifetime, so two draws in one pass need two
    /// disjoint fields (the same aliasing rule the scalebar's own two quads
    /// document below).
    caption_back_quad: Quad,
    /// Miniature dark logo (from `LOGO_DARK_PNG`) flanking each session badge
    /// in the bottom strip, plus its native `(w, h)` for aspect-ratio sizing.
    /// `None` when the embedded PNG fails to decode — purely cosmetic, the
    /// strip renders unchanged without it.
    logo_quad: Option<(Quad, u32, u32)>,
    /// Small wordmark logo (from `LOGO_WORDMARK_PNG`) drawn at the top of the
    /// nav pane, plus its native `(w, h)` for aspect-ratio sizing. `None` when
    /// the embedded PNG fails to decode.
    wordmark_quad: Option<(Quad, u32, u32)>,
    /// Combined multiplier (`cli.scale * window.scale_factor()`) applied to
    /// all text + cell metrics. Captured once at startup; ScaleFactorChanged
    /// is currently ignored.
    scale: f32,
    cell_w: f32,
    cell_h: f32,
    chrome_origin_x: f32,
    chrome_origin_y: f32,
    /// `--capture <path>`: render to PNG and exit. Set means we keep
    /// requesting redraws until `frame_counter == CAPTURE_FRAME` so the
    /// transport task has time to push events.
    capture_path: Option<PathBuf>,
    /// Ctrl+Shift+S selfie: a pending whole-window capture target. Set by the
    /// keybind, consumed by the render loop on the next frame — unlike
    /// `capture_path`, the FE does NOT exit after (it's a live screenshot).
    selfie_pending: Option<PathBuf>,
    /// `--capture-preview <relpath>`: project-relative path to fire
    /// `preview.get` for as soon as the first Files-mode `tree.root` lands.
    /// One-shot — consumed on first dispatch.
    capture_preview: Option<String>,
    /// True when `--capture-preview` was supplied. Persists past the
    /// one-shot `capture_preview` consumption so the readback frame
    /// delay knows to wait for the extra round-trip + MathJax.
    capture_preview_armed: bool,
    /// Explicit `--capture-delay-ms` override (0 = auto). When non-zero,
    /// the readback frame is `delay_ms * 60 / 1000` regardless of
    /// `capture_preview_armed`.
    capture_delay_ms: u32,
    /// `--capture-cycle <N>`: simulate N presses of Shift+ArrowRight (or
    /// `|N|` of Shift+ArrowLeft when negative) on the first `workspace.list`
    /// reply. One-shot — consumed on first application so a re-fetch
    /// later doesn't re-cycle. Zero leaves the active workspace alone.
    capture_cycle: i32,
    frame_counter: u32,
    /// Runtime multiplier on top of the startup `scale`. `1.0` = no
    /// change; `>1.0` = bigger fonts and cells; `<1.0` = smaller.
    /// Bumped via `Ctrl+=` / `Ctrl+-` (reset by `Ctrl+0`). Affects
    /// chrome cell metrics, the TextLayer's per-line metrics, and the
    /// preview's flowed-text buffer simultaneously (user picked
    /// "Global only" — same change applies to every pane).
    text_scale_mult: f32,
    // ui/app: the event fan-in, frame pacing, the exit flag and the harness's startup actions.
    /// Drained at the top of every redraw; every host's transport task
    /// pushes here, tagged with its own `HostKey` (ADR 0042 L2a fan-in).
    evt_rx: std::sync::mpsc::Receiver<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>,
    #[cfg(all(test, feature = "test-window-progress"))]
    native_progress: Option<Arc<std::sync::Mutex<NativeProgressLedger>>>,
    /// One-shot from `--auto-expand`; consumed after `pending_initial_selection`
    /// lands. Fires the same outgoing request the Enter/Right key would,
    /// so capture tests can verify expanded states.
    pending_auto_expand: bool,
    /// One-shot from `--auto-pin`. Same trigger conditions as
    /// `pending_auto_expand`: fires after the initial cursor selection
    /// has landed and the tree isn't empty. Lets `--capture` exercise
    /// the C2 pin sigil and `[pinned *]` preview-title chrome.
    pending_auto_pin: bool,
    /// `--demo-function-methods <module>:<name>` one-shot. When the row
    /// `modules:<module>:<name>` appears in the tree (after a prior col-2
    /// expansion has landed), the chrome moves the cursor onto it and
    /// fires `function.methods`. Consumed once it triggers.
    pending_demo_function_methods: Option<(String, String)>,
    /// `--demo-repl-eval <code>` one-shot: submit through the FE's own
    /// path once the initial tree has landed, then open the REPL drawer.
    /// Externally-dispatched evals are dropped by design (frames only
    /// render for self-created `repl_log` entries), so the harness enters
    /// by the front door.
    pending_demo_repl_eval: Option<String>,
    /// `--start-path <relpath>` walk state (files-mode). While pending, each
    /// tree update either lands the cursor on the target file's row (done)
    /// or expands the deepest existing collapsed ancestor directory and
    /// waits for its children splice. Consumed on arrival or dead end.
    pending_start_path: Option<String>,
    /// Ancestor row id we last fired `tree.children` for during the
    /// `--start-path` walk. Expansion flips `row.expanded` only when the
    /// reply lands, so without this memo the walk would re-fire the same
    /// request on every redraw in between.
    start_path_fired: Option<String>,
    /// Harness instance (`--ephemeral`, or any `--capture` run): never the
    /// user's primary FE on this host, so it must not touch the per-host
    /// shared state — no resume-state / `fe-state.json` writes, no state
    /// restore, and no consumption of `fe-commands/` or the relaunch
    /// sentinel (both watchers DELETE what they read — a harness eating
    /// the primary's relaunch signal or control command is the B8 bug
    /// class). Single-writer rule for multi-FE hosts.
    ephemeral: bool,
    /// True after a successful capture; the WindowEvent handler reads this
    /// next event-loop iteration and calls `event_loop.exit()`.
    should_exit: bool,
    /// The App's one process backstop, shared so the final decision arms it.
    exit_deadline: ExitDeadline,
    /// Frame-rate cap state. `request_redraw` from event handlers and the
    /// transport task queue `RedrawRequested`; if we'd draw twice within
    /// `FRAME_BUDGET`, the second one sets `dirty` and `about_to_wait`
    /// reschedules at the next frame boundary so a burst (paste, PTY echo
    /// storm, LLM token stream) collapses into one frame.
    dirty: bool,
    last_frame_at: Option<std::time::Instant>,
    /// One-shot: focus + raise the window on the first rendered frame.
    /// A focus request made at window-creation time (before the window
    /// is shown / before the first paint) is widely ignored by window
    /// managers — Windows' foreground-lock and macOS both restrict it.
    /// Deferring to the first frame is the portable way to land focused
    /// on launch / after an ADR-0017 self-relaunch. When the OS blocks
    /// focus-stealing outright we fall back to `request_user_attention`
    /// (taskbar flash / dock bounce / urgent hint). Cleared after use.
    focus_on_first_frame: bool,
    // ui: the per-host request senders that send and send_to route through.
    /// Push-side of every host's GPU→transport channel, in connection
    /// (display) order — ADR 0042 L2a's generalisation of the old single
    /// `req_tx`. Empty in offline mode (no transport spawned), in which
    /// case `send`/`send_to` no-op with a chrome hint. Use `self.send(req)`
    /// (routes to `active_host`) or `self.send_to(host, req)` (routes to a
    /// specific row's host) rather than reading this directly.
    conns: Vec<(
        crate::net::dial::HostKey,
        tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
    )>,
    // net/hosts.rs (fe-net): the per-host connection table.
    /// fe-net's per-host connection table (F/net/hosts.rs).
    hosts: crate::net::hosts::HostTable,
    // lease.rs (lifecycle): the window's leases and its leave.
    leases: Arc<crate::lease::Leases>,
    /// The not-ended line a lease grant reported once a frame showed it, and
    /// when it stops showing.
    not_ended_shown: Option<(String, std::time::Instant)>,
    /// Set once the window is on its way out and waiting for the daemons' acks.
    leaving: Option<crate::lease::Leaving>,
    // ui/chrome: the status line, focus, pane rects, maximize and nav spill.
    /// One-line status string for the chrome.
    status: String,
    /// While `Some(t)` and `now < t`, the status line is holding a pushed
    /// notify (op::FE_COMMAND `notify`) and `rebuild_connection_status` won't
    /// clobber it — so a toast survives a workspace switch for a few seconds
    /// instead of being overwritten instantly (which otherwise made a pushed
    /// notify un-seeable while the user roamed workspaces). Cleared + the
    /// status rebuilt once elapsed (in `about_to_wait`, on the idle tick).
    notify_sticky_until: Option<std::time::Instant>,
    /// Selected-session contrast lever (`--contrast-mode`, ADR 0023). `false`
    /// = "bright" (default): the selected/active row pops by going brighter +
    /// bold. `true` = "dim": non-selected rows are dimmed so the selection
    /// pops by contrast. Applied in both the nav rows and the bottom strip.
    contrast_dim: bool,
    /// Cached battery readout for the top-right chrome (e.g. `85%` or `+72%`
    /// while charging). `None` means no battery present / query failed — we
    /// render nothing in that case (never a fake `0%`). The OS query is not
    /// free, so it's refreshed at most once per `BATTERY_QUERY_INTERVAL`; the
    /// clock ticking every second reuses this cached value in between.
    battery_label: Option<String>,
    /// When the cached `battery_label` was last (re)computed. `None` forces a
    /// query on the first paint.
    last_battery_query: Option<std::time::Instant>,
    /// Window-wide focus, prompt editing and close notices; runtime state only.
    focus_prompts: FocusPrompts,
    /// The four pane content rects from the most recent redraw, cached
    /// so keyboard handlers can size scroll steps to a real viewport
    /// (`PgUp/PgDn`, `Ctrl+u/d`). Updated at the end of every redraw.
    pane_rects: PaneRects,
    /// Runtime drawer, layout flags, Help browsing, origin and startup requests.
    layout_help: LayoutHelp,
    /// Transient nav spill (`[nav] spill_ms`): while the user is actively
    /// moving the nav cursor, nav rows whose text overflows the column
    /// float their FULL text over the preview pane's left edge (overlay
    /// text layer + backing strip — pane geometry never moves), vanishing
    /// once this deadline passes with no further moves. `None` = spill
    /// not active. Expiry repaint rides the `about_to_wait` idle tick,
    /// same as `notify_sticky_until`.
    nav_spill_until: Option<std::time::Instant>,
    /// The spill rows collected by the LAST draw-closure run — the
    /// `media_paint_targets` pattern: the closure writes cell-space
    /// segments here, the render-pass tail reads them for the backing
    /// quads and the overlay text prepare. Empty whenever the spill is
    /// inactive or nothing overflows.
    nav_spill_segments: Vec<NavSpillSeg>,
    /// Last (mode, workspace, tree cursor, picker cursor) the spill
    /// trigger saw — a change between frames is what (re)arms the timer.
    /// Frame-side detection so every cursor mover (keys, wheel, mode and
    /// workspace switches) triggers uniformly without touching each input
    /// path. `None` until the first frame, which deliberately does not
    /// arm (boot isn't a user move).
    nav_spill_cursor: Option<(Mode, Option<String>, usize, Option<usize>, u64)>,
    // ui/chrome/strip: the session strip's scroll and wheel animation.
    /// Session-strip horizontal scroll, in strip-local pixels (the
    /// strip-local x that maps to screen-center). `None` = uninitialized
    /// → snap to the active session's center on first paint (no slide-in).
    /// Eases toward the active session's center each frame; while it hasn't
    /// settled, `redraw` sets `dirty` so the frame loop keeps animating.
    strip_scroll_px: Option<f32>,
    /// Timestamp of the last strip-animation frame, for frame-rate-
    /// independent easing. `None` when settled (so the next switch starts
    /// a fresh ease rather than seeing a stale dt).
    strip_anim_last: Option<std::time::Instant>,
    /// Brand-wheel spin gimmick (see `WHEEL_*`): current rotation of the
    /// bottom-strip bow wheels (radians), the live angular velocity a
    /// workspace cycle flicks it with, and the last spin-frame timestamp for
    /// frame-rate-independent decay (`None` when at rest).
    wheel_angle: f32,
    wheel_vel: f32,
    wheel_anim_last: Option<std::time::Instant>,
    // ui/input: the last key, pointer state, key bindings and the help drawer.
    /// Label of the most recent key press (for chrome feedback). `None` until
    /// the user hits a key; modes-mode + tree navigation hang off the same
    /// keyboard input plumbing once they land.
    last_key: Option<String>,
    /// Latest cursor position in physical pixels — winit only
    /// delivers `CursorMoved` on motion, so a click immediately after
    /// focus would otherwise have no position to anchor to.
    cursor_px: (f32, f32),
    /// Fractional wheel-row accumulator. Precision touchpads and smooth
    /// wheels emit small sub-row deltas; truncating each event to i32
    /// drops everything until the user flicks hard. Accumulating here
    /// and only applying whole rows lets gentle scroll work the way the
    /// user expects.
    wheel_residue_y: f32,
    /// Resolved keybindings (defaults overlaid with the user's
    /// `keybindings.toml` if present). See `ui/input/keybindings.rs` for the
    /// file format and discovery order. Read-only once loaded — the
    /// chrome doesn't reload mid-session.
    bindings: KeyBindings,
    help_back_quad: Option<(u8, Quad)>,
    // ui/control: the fe-commands queue and the fe-state.json signature.
    /// FE control commands (ADR 0019) enqueued by the command-file watcher
    /// thread (the producer) and drained on the main thread in `window_event`
    /// (the consumer), so dispatch runs the same code paths as the keybinds.
    fe_commands: Arc<std::sync::Mutex<std::collections::VecDeque<FeCommand>>>,
    /// Hash of the last `fe-state.json` we wrote (ADR 0019), so the readback
    /// file is only rewritten when the observable state actually changes.
    fe_state_sig: Option<u64>,
    // ui/nav: the active tree and mode, cursor-driven preview firing, request generations.
    /// Files-mode tree, flattened for chrome rendering. Updated by
    /// `tree.root` / `tree.children` events; navigated by arrow keys.
    tree: TreeView,
    /// Which root tree the left pane is currently showing. `f`/`m` keys
    /// switch this and fire the corresponding wire request.
    mode: Mode,
    /// Absolute path of the most recent `project.scan`'s `project_root`.
    /// The chrome strips this prefix off the absolute file paths each
    /// Modules-mode entry carries to synthesize the `files:<relpath>`
    /// node id that `preview.get` expects. Reset on each scan reply.
    scan_project_root: Option<String>,
    /// Per-(mode, scope) parking lot for every nav tree that is NOT the
    /// active one (`self.tree` IS the active slot). Replaces the old
    /// `files_tree_workspace` provenance side-stamp + per-consumer
    /// staleness guards: installs route by the reply's key, so the active
    /// view can only ever hold the active (mode, workspace)'s tree — a
    /// foreign tree is structurally impossible rather than detected.
    tree_store: TreeStore,
    /// Persistent scroll offset for the nav pane (vim-style scrolloff
    /// behaviour). Updated each frame from the cursor's position relative
    /// to the current viewport: when the cursor moves into the bottom
    /// 1/3 of the pane going down, scroll keeps it stationary there;
    /// same on the way up. At the body's edges the cursor falls through
    /// to the actual top/bottom row. Kept on State so the cursor
    /// position alone doesn't determine the scroll — direction of motion
    /// matters.
    tree_scroll: u16,
    /// Where the nav cursor lands and when its follow fires: the settle debounce, the startup, relaunch and
    /// reconnect placements, the README default, the pin and the per-row project-scan clock.
    nav_cursor: NavCursor,
    /// Request clocks, the fired target and its re-raster, concept follow, the driven-preview hold, and the crop,
    /// scale-save and `--roi` aim transactions.
    preview_requests: preview::requests::PreviewRequests,
    // ui/nav/files: nav prompts, the reveal walk and file transfers.
    /// Node id of a file or directory just created via the Ctrl+N prompt,
    /// awaiting its `file.write` (file) or `dir.create` (dir) reply. When
    /// that reply lands matching this id, we refresh the parent dir's
    /// listing so the new entry appears, then clear this. `None` outside a
    /// create round-trip.
    pending_created_node_id: Option<String>,
    /// Node id of a file just deleted via the Ctrl+D confirm prompt, awaiting
    /// its `file.delete` reply. When that reply lands matching this id, we
    /// refresh the parent dir's listing so the row vanishes, then clear this.
    /// `None` outside a delete round-trip.
    pending_deleted_node_id: Option<String>,
    /// Deep-path reveal target for a driven open (`files:<relpath>`), set when
    /// the BE opens a file whose row isn't visible yet because its ancestor dirs
    /// aren't expanded. `drive_reveal_step` expands one ancestor per
    /// `tree.children` round-trip until the row materializes, then lands the
    /// cursor on it (so the nav header + viewport follow the preview body — one
    /// command drives both panes; the BE never issues a separate cursor move).
    /// `None` when no reveal is in flight.
    pending_reveal: Option<String>,
    /// The ancestor dir id (`files:<relpath>`) whose `tree.children` we've
    /// already requested for the in-flight `pending_reveal` and are awaiting.
    /// Guards against re-requesting the same in-flight level when unrelated
    /// `tree.children` replies re-enter `drive_reveal_step`. Cleared/advanced as
    /// each level resolves.
    reveal_awaiting: Option<String>,
    /// For a reveal whose deepest *expanded* ancestor dir doesn't contain the
    /// target, the `(target_id, ancestor_id)` pair we already force-refreshed
    /// once. A brand-new file (an agent wrote it *after* the dir was last
    /// listed) is absent from the cached children, so `drive_reveal_step`
    /// re-fetches that dir (a fresh `list_dir` stat surfaces it) instead of
    /// giving up — one loopback round-trip, sub-second, so generate→preview and
    /// badge→navigate land on fresh files. Keyed by `(target, ancestor)` so it
    /// self-scopes to this reveal and can't loop: if the same dir is refreshed
    /// and the target is STILL absent it's genuinely gone — stop. Cleared when
    /// the cursor lands.
    reveal_refetched: Option<(String, String)>,
    /// One-shot cursor reveal for a preview driven via a *workspace switch*
    /// (#4 fix). Cross-ws force-show previews and #1's persisted `nav.preview`
    /// are consumed in `switch_to_workspace`, which fires the preview body
    /// before the switched-to workspace's `tree.root` has loaded — so the
    /// target row isn't in `tree.rows()` yet and the cursor can't land inline.
    /// We stash the `files:` node id here and apply it when that workspace's
    /// `tree.root` reply arrives (re-using `drive_reveal_step`, so a top-level
    /// row lands directly and a nested one expands its ancestors). Without it
    /// the preview pane updates but the nav cursor stays on the old row — the
    /// desync the maintainer hit. `None` once consumed.
    pending_switch_reveal: Option<String>,
    /// In-flight `file.upload`, if any. The chrome drives chunk flow control:
    /// it sends chunk 0 in `start_upload`, then sends the next chunk on each
    /// non-`done` `FileUploadAck`. `None` when no upload is running.
    upload: Option<UploadState>,
    /// The multi-file batch the in-flight `upload` belongs to, if any. Holds the
    /// not-yet-started files and the shared destination; `Some` for the whole
    /// duration of a multi- (or single-) file upload, cleared when the last
    /// file's ack lands. Guards `u` against starting a second batch mid-run.
    upload_batch: Option<UploadBatch>,
    // ui/session: the active (host, workspace), workspace lists and caches, picker, snapshots.
    /// The selected row, `(host, workspace_id)`, and its read-mark dwell (ADR 0014, ADR 0042 L2a). A `None`
    /// workspace is the host daemon's default; the host is the connection every current-view operation targets,
    /// first the connection `resolve_default_host` names (local-first, then --dial argument order). A person's
    /// switch persists it to the per-host resume file so a restart resumes there.
    selected: SelectedRow,
    /// Authoritative host listings and canonical identity translation, owned by session/row_directory.rs.
    row_directory: RowDirectory,
    /// Private listed-row projections, activity order and transition flashes.
    row_presentation: RowPresentation,
    /// Short hostname from the hello response, kept alongside the
    /// daemon's project_root basename so the chrome can rebuild the
    /// "connected · host:workspace · rev N" status line every time the
    /// active workspace changes — not just at connect time. ADR 0042 L2a:
    /// updated only from events whose host is `active_host` — this field
    /// (and its three siblings below) describe the active connection's
    /// status line, not every connection; `host_connected` above is the
    /// per-host status the Sessions tree needs.
    host: Option<String>,
    /// Project_root basename from the hello response. Used as the label
    /// whenever `active_workspace_id` is None; explicit slugs use presentation labels.
    daemon_root_basename: Option<String>,
    /// Full project_root path from the hello response. Joined with the
    /// `files:<rel>` node id by `copy_navtree_path` so Ctrl+C in NavTree
    /// yields the absolute backend-side path (paste-into-shell utility).
    daemon_project_root: Option<String>,
    /// The connected backend's product version (`HelloRes::app_version`),
    /// painted beside the FE's own version on the bottom chrome edge.
    /// `None` before the first hello, or when the daemon is old enough not
    /// to send the field. Deliberately NOT cleared on disconnect — a stale
    /// "what we last talked to" reads better than a blank half, and the
    /// transport state is already called out in the nav status row, so the
    /// stamp doesn't need to duplicate it.
    backend_version: Option<String>,
    /// Highest revision seen in any frame, surfaced in the status line.
    /// Tracked separately so post-connect events (workspace switches,
    /// preview.changed bumps) don't render a stale rev.
    last_revision: u64,
    /// Runtime results, attempts and presentation certificates for canonical rows.
    result_ledger: ResultLedger,
    /// Sessions-mode workspace picker (ADR 0014). `Some(state)` while
    /// the user is browsing a directory tree to pick the project_root
    /// of a new workspace; `None` outside the picker. Supersedes the
    /// older label-only prompt that fired `tmux.create_session` with
    /// a hardcoded `$SOT_PROJECTS_ROOT/<label>` cwd.
    workspace_picker: Option<WorkspacePicker>,
    /// Per-row parked views, captured when the user switches away from a
    /// row and restored when they switch back. Keyed by `WsKey` (ADR 0042
    /// L2a, Codex review PR #163: bare-slug keying let "sot" on host A and
    /// "sot" on host B share a slot -- `"<default>"` for the
    /// daemon-default within a host).
    parked_views: ParkedViews,
    /// REPL row records, drafts, history, eval routing and lifecycle.
    repl_model: ReplModel,
    /// The last `fe.sessions` projection this frontend sent out
    /// (session-listing brief decision 2) — `None` until the local
    /// daemon's first `workspace.list` reply. Compared against the next
    /// projection so a declaration is re-sent only when it actually
    /// changed (edge-driven, no timer); also what a reconnecting hub is
    /// resent verbatim in the `Connected` arm, since its own fresh
    /// connection remembers nothing from before.
    last_declared_sessions: Option<Vec<sot_protocol::DeclaredSession>>,
    /// This window's last real press and last `fe.presence` send (`report_presence`, `presence_due`): ONE clock gates
    /// the fan-out to every connected daemon, and idle input sends nothing at all (no timer, no heartbeat).
    presence: PresenceClock,
    // ui/drawer: the monitor drawer.
    /// Ctrl+M server-monitor drawer (ADR 0020): per-host metric ring + SVG
    /// chart renderer. `monitor_quad` is the rasterised chart painted into
    /// the drawer rect via the resvg→wgpu-quad path (same as MathJax);
    /// `monitor_rect_px` is the drawer's pixel rect from the last layout
    /// pass; `monitor_dirty` requests a re-render when the readings change
    /// (a change of size, text scale or mark redraws by itself:
    /// `MonitorView`'s `drawn`).
    monitor_view: crate::ui::drawer::monitor::MonitorView,
    monitor_quad: Option<Quad>,
    monitor_rect_px: ScreenRect,
    monitor_dirty: bool,
    /// The declared hub's name (topology `hub = "<host>"`), loaded once at
    /// startup the same way `selfupdate.rs` reads the topology. `None` when
    /// no `hosts.toml` declares one. Names the monitor drawer's subscribe
    /// target (`monitor_host`) — the drawer means "the fleet's record", so
    /// it asks the hub, not whichever connection sorts first.
    monitor_hub: Option<String>,
    /// Primary monitor aspect ratio captured at startup (width /
    /// height). Used to resolve `settings.preset = "auto"` to a named
    /// preset. Locked for the session — resizing the window doesn't
    /// re-pick a different preset; the user explicitly avoided
    /// in-session reflow.
    monitor_aspect: f32,
    /// Window-wide REPL images, pixel geometry and line-build anchor.
    repl_presentation: ReplPresentation,
    // ui/drawer/terminal: the terminal drawer's backends.
    /// Local PTY terminal hosting the OS shell (G2). Lazily spawned the
    /// first time the Terminal drawer opens (Ctrl+T); a separate field
    /// from the ratatui `terminal` so the draw closure's `self.terminal`
    /// borrow and this terminal's `screen()` borrow are disjoint.
    local_term: Option<crate::ui::drawer::terminal::pty::LocalTerminal>,
    /// Last `(cols, rows)` the local terminal's PTY was sized to. `None`
    /// until the drawer rect is first observed; drives resize-on-change
    /// (mirrors the agent pane size).
    term_size: Option<(u16, u16)>,
    /// Local repo root (`$SOT_REPO_DIR`, set by the supervisor). Used as
    /// the Terminal drawer's working directory, so the plain shell it spawns
    /// starts in the project root. `None` when launched outside the
    /// supervisor (then the shell inherits the frontend's cwd). ADR 0017.
    repo_dir: Option<std::path::PathBuf>,
    // ui/agent_pane: selected client, warm storage, screen, input and receipt owner.
    agent_pane: AgentPane,
    // ui/preview: the preview pane's buffers, concept slot, scroll and overlays.
    preview_svg: Option<Quad>,
    /// Tree-sitter-backed syntax highlighter for the markdown preview's
    /// fenced code blocks (and, later, the editor pane). Constructed
    /// once per `State` because `HighlightConfiguration::new` compiles
    /// the per-language highlight query — moderately expensive vs
    /// the per-redraw highlight call itself.
    highlight_service: crate::ui::preview::markdown::highlight::HighlightService,
    preview_md: MarkdownPreview,
    /// Pixel rect of the markdown pane from the most recent ratatui layout
    /// pass; cached so we can re-shape on resize without re-running layout.
    md_rect_px: ScreenRect,
    concept: Option<ConceptInfo>,
    /// Definition line (1-indexed) the in-flight `preview.get` should
    /// anchor to once it lands — set from the selected modules-mode row's
    /// `line` payload so the code preview scrolls the item (its docstring
    /// if present, else the definition) to the top of the pane instead of
    /// showing the containing file from line 1. `None` for files-mode rows
    /// (which carry no `line`) → preview opens at the top as before.
    /// Consumed once by the Preview reply handler.
    preview_anchor_line: Option<u32>,
    /// The definition line the *currently shown* code preview is anchored to.
    /// Lets `maybe_fire_preview` re-anchor without a re-fetch when the cursor
    /// moves between items in the SAME file (common within a module) — the
    /// same-node early-return otherwise skips re-anchoring, so the preview
    /// stays parked on the first item. Compared against the new selection's
    /// line so we re-anchor only on a real change, never fighting the user's
    /// manual scroll on a stable selection.
    preview_anchored_to: Option<u32>,
    /// Shaped annotation body for the latest `concept.read` reply, ready
    /// for the chrome's concept pane to render. `None` when the cursored
    /// row has no annotation; rebuilt on every event so we don't pay the
    /// markdown shape cost per frame.
    preview_concept: Option<MarkdownPreview>,
    /// Pixel rect of the concept pane from the most recent layout pass.
    /// Cached so `resize` triggers only on actual shape changes.
    concept_rect_px: ScreenRect,
    /// Scroll offset (rows from the top) of the preview pane's flowed
    /// text. 0 = top of the markdown body; positive = scrolled down.
    /// Drives an upward pixel shift of the cosmic-text TextArea via
    /// `ExtraArea::scroll_y_px`. Image-only previews (PNG/SVG) ignore
    /// this. Clamped at render time to total_layout_lines minus visible
    /// rows so the user can't scroll past the bottom of the content.
    preview_scroll: u16,
    /// ADR 0030 §2 / ADR 0042 L2a: a persistent, blocking "update needed"
    /// message shown when a host's daemon refuses the handshake on a
    /// protocol-version skew. Keyed by `HostKey` — each host's own hello
    /// answers for itself — so ONE stale/optional remote can't block the
    /// whole UI while every other (healthy) host works fine; only
    /// `active_host`'s entry is projected to the blocking overlay (see
    /// `rebuild_fatal_overlay`/`show_fatal`). A host's entry is removed on
    /// its own next successful `Connected` (not every host's). `preview_fatal`
    /// is the shaped buffer for whichever host is CURRENTLY active, rebuilt
    /// on set + on resize + on an active-host switch.
    hello_refused: HashMap<HostKey, String>,
    preview_fatal: Option<MarkdownPreview>,
    /// The installed source: bytes, their node, their scale and the markdown origin, repainted from on a rescale or
    /// a row's restore without a round trip.
    preview_source: preview::source::InstalledPreviewSource,
    // ui/preview/image: the image view, ROI, pages, scalebar, captions and figures.
    preview_png: Option<Quad>,
    /// Zoom multiplier for the PNG preview pane — 1.0 fits the image to
    /// the pane (current letterbox behaviour); >1.0 samples a 1/zoom-wide
    /// sub-rect of the texture so detail becomes legible.
    preview_png_zoom: f32,
    /// Pan offset in physical pixels of the zoomed canvas relative to
    /// the pane centre. `(0, 0)` keeps the canvas centred in
    /// `preview_rect`; arrow keys nudge by a fraction of the pane.
    /// Clamped at render time so the canvas covers the pane in whichever
    /// axes it's larger than the pane — the user can't pan past the
    /// image edge and reveal empty pane.
    preview_png_pan_px: (f32, f32),
    /// Natural dimensions (w, h) of the currently-loaded PNG, used as
    /// part of the cache key so flipping between same-size renders in
    /// the same directory (e.g. successive timesteps of a plot) keeps
    /// the same view (see `preview_png_cache`). `None` until a PNG has
    /// loaded.
    preview_png_dims: Option<(u32, u32)>,
    /// Dimensions of the shown raster as SERVED, before the GPU-fit downsample
    /// that `decode_and_fit` applies to images exceeding
    /// `max_texture_dimension_2d`. Equals `preview_png_dims` for everything
    /// under the cap. The ADR-0034 scalebar keys off THIS: the wire's
    /// `nm_per_px` describes served pixels, so measuring against the shrunken
    /// texture width mis-scales the bar by `src/texture` (a 20000 px raster
    /// capped to 16384 reads ~1.22x short). Re-derived on every decode, so it
    /// stays correct across workspace-snapshot repaints.
    preview_png_src_dims: Option<(u32, u32)>,
    /// The visible region of the current image preview, in *source-image
    /// pixel* coords (ADR 0022). Recomputed each draw from zoom/pan/letterbox
    /// when an image is shown; `None` when the preview isn't a croppable
    /// image. Consumed by `capture_roi` (the `C` hotkey / `capture_roi`
    /// fe-command) and surfaced in `fe-state.json` so the LLM pane knows what
    /// the user is zoomed into.
    preview_roi: Option<PreviewRoi>,
    /// Per-directory + per-dimensions cache of the last visible SOURCE-PX
    /// ROI, so switching among same-sized image renders in one directory
    /// restores the previous view rather than snapping back to fit.
    /// Mixed-size images miss the cache and start at fit-to-pane.
    ///
    /// Source px, not `(zoom, pan_px)`: screen-px values are only valid
    /// against the image rect they were measured in, and that rect moves —
    /// the caption band resizes it per file (and the pane itself can resize
    /// between save and restore). A screen-px carry between a captioned and
    /// an uncaptioned same-size neighbor restored an offset, wrongly-
    /// magnified view. An ROI re-solved against the live rect at consume
    /// time (`pending_roi_restore`) is geometry-independent by construction.
    /// Written through from the render pass each draw — a keystroke-time
    /// save recorded the previous frame's view — and only for image node
    /// ids (via `preview_roi`'s gate), which also stops PDF page turns
    /// (same node id ⇒ same key) from restoring one page's view onto
    /// another when a page turn is specified to reset to fit.
    preview_png_cache: HashMap<(String, (u32, u32)), RoiRect>,
    /// One-shot deferred view restore: a cache hit at preview install can't
    /// solve immediately — there's no live geometry there, and
    /// `caption_band_px` at install time still describes the PREVIOUS
    /// node's band — so the hit is parked as `(node_id, roi)` and consumed
    /// in the render pass next to the `--roi` aim solve, against the
    /// current `image_rect`. An aim consumed for the same install wins over
    /// the carry, and a carry restore never emits the ADR-0025
    /// `preview_roi_applied` echo (that event is the CLI contract for
    /// explicit aims). Node-guarded and overwritten on every raster
    /// install, so a stale entry is inert.
    pending_roi_restore: Option<(String, RoiRect)>,
    /// Pagination of the preview the pane is *showing* (ADR 0021):
    /// `(page, page_count)` from the reply's extras. `None` for
    /// unpaginated previews. Drives the `n`/`p` page-turn keys and the
    /// title's `p N/M` suffix; never inspected per-file-type — any plugin
    /// that reports page extras gets the transport for free.
    preview_page: Option<(u32, u32)>,
    /// Zoom level the *current* page bitmap was rasterized for (1.0 = fit
    /// to pane). Zooming in past this re-requests the page at the larger
    /// pixel size so rasterized text stays crisp instead of stretching the
    /// fit-sized bitmap (ADR 0021 deferred follow-up). Reset to 1.0 on a
    /// fresh page/file.
    preview_page_raster_zoom: f32,
    /// One-shot: the next `image/png` render is a zoom re-raster of the
    /// page already on screen, so the quad swap must preserve zoom/pan
    /// rather than reset to fit. Set by the Preview handler, consumed in
    /// `render_preview_source`.
    preview_reraster_keep_view: bool,
    /// Scalebar overlay toggle (ADR 0034, `b`). Default off; sticky across
    /// navigations (renders only when the installed scale is present, so it can
    /// stay armed while browsing a mix of scaled / unscaled rasters).
    scalebar_on: bool,
    /// The shaped scalebar label buffer (e.g. `500 nm`), rebuilt each frame
    /// from the adaptive bar length by `build_scalebar` (shaped OUTSIDE the
    /// render pass). `None` when the bar isn't drawn.
    scalebar_label: Option<crate::ui::preview::markdown::MarkdownPreview>,
    /// Agent-supplied figure captions, sticky per (host, listed workspace slug, file).
    /// Commands write the resolved target key and rendering reads the corresponding
    /// active row key. The store is shared across snapshots because its keys carry
    /// the host and workspace identity.
    preview_captions: CaptionStore,
    /// The shaped caption buffer for the image on screen, rebuilt each frame by
    /// `build_caption` (shaped OUTSIDE the render pass, like `scalebar_label`).
    /// `None` when no caption is drawn.
    caption_label: Option<crate::ui::preview::markdown::MarkdownPreview>,
    /// Height in px of the caption band the LAST frame reserved (0.0 for none).
    /// Published by the render pass purely so the KEYBOARD zoom/pan handler can
    /// reach it: that handler runs outside the render pass, rebuilds the pane
    /// rect from cells, and would otherwise compute its zoom ceiling against the
    /// unreduced pane — making the reachable max zoom depend on whether a
    /// caption happens to be set (verified 2.9%–6.4% short on a
    /// height-constrained image). Band height is only knowable after the caption
    /// is shaped, so it has to be stashed rather than recomputed.
    caption_band_px: f32,
    /// Cache of fetched markdown-figure bitmaps keyed by the literal
    /// URL string that appeared in the source (`![](url)`). Populated
    /// by `IncomingEvt::FigureLoaded`; consumed by the markdown render
    /// path to paint quads over the FFFC placeholders the walk
    /// reserves for each figure. Survives navigation so re-opening a
    /// doc with the same figure doesn't re-roundtrip.
    figure_cache: std::collections::HashMap<String, FigureCacheEntry>,
    /// In-flight figure.get requests, keyed by the same URL string.
    /// Prevents duplicate dispatch when a markdown doc references the
    /// same figure multiple times or when the user re-loads a
    /// partially-fetched doc.
    figure_pending: std::collections::HashSet<String>,
    /// URLs whose figure fetch/decode terminally failed (bad bytes,
    /// unresolvable local path). Reported to the markdown walk as 0-size
    /// metrics so the layout collapses their reservation to the compact
    /// text fallback instead of holding an empty FIGURE_BLOCK_H_DEFAULT
    /// box forever.
    figure_failed: std::collections::HashSet<String>,
    // ui/preview/markdown: tables, math and token caches, and reflow.
    /// Horizontal scroll for wide markdown tables. Shared across every
    /// `MediaBlock::Table` on the current preview — Windows's (e) work
    /// item ships Path 1, where each table buffer is at natural width
    /// and we shift it left by this many pixels then let `TextBounds`
    /// clip the overflow to the preview pane. One scroll var keeps the
    /// state model trivially small; in practice a markdown doc only
    /// ever has one wide table on screen at a time. h/l (Preview focus)
    /// step ±~1 cell; Shift+wheel-Y also adjusts. Reset to 0 whenever
    /// `preview_md` is rebuilt so navigating between docs starts fresh.
    md_table_scroll_px: f32,
    /// Per-table cosmic-text buffers hosted as ExtraAreas. Indexed in
    /// `media_blocks` Table-encounter order so multiple tables on one
    /// page each get their own slot. `rendered` is the source text the
    /// buffer was built from; on every redraw the chrome compares it
    /// against the current `media_blocks` so navigating to a doc with
    /// the same table count but different content still rebuilds.
    /// Each buffer is laid out at huge width (no soft-wrap) and
    /// `natural_w_px` is the measured max line width — used by the
    /// scroll clamp.
    table_buffers: Vec<TableBufferEntry>,
    /// Cache of MathJax-rendered SVGs keyed by `(latex, display)`.
    /// Populated by `IncomingEvt::MathRendered`; consumed by the
    /// markdown render path to paint quads over FFFC placeholders.
    /// Survives navigation so re-opening a doc with the same math
    /// doesn't re-roundtrip.
    math_cache: std::collections::HashMap<(String, bool), MathSvg>,
    /// In-flight math.render requests, same key shape as `math_cache`.
    /// Prevents duplicate dispatch when a markdown doc has the same
    /// equation twice or when the user re-loads a partially-fetched
    /// doc before the first replies land.
    math_pending: std::collections::HashSet<(String, bool)>,
    /// Per-fence semantic-overlay cache keyed by `(lang, source_hash)`.
    /// Populated by `IncomingEvt::MarkdownTokens`; consumed by the
    /// CodeBlock walk to overlay tree-sitter base spans with backend-
    /// derived semantic spans (function-def, call-site, type, etc.).
    /// Survives navigation so re-rendering identical fences in another
    /// doc (Julia ecosystem reuses the same snippets) skips the round
    /// trip.
    markdown_token_cache:
        std::collections::HashMap<(String, u64), Vec<crate::net::transport::MarkdownToken>>,
    /// In-flight markdown.tokenize requests, same key shape as
    /// `markdown_token_cache`. Prevents duplicate dispatch when the
    /// same fence appears twice in a doc or the user re-renders before
    /// the reply lands.
    markdown_token_pending: std::collections::HashSet<(String, u64)>,
    /// Set by the MathRendered event handler when a fresh SVG enters
    /// `math_cache`. The redraw loop checks this and rebuilds
    /// `preview_md` from the installed source so the markdown walk can size
    /// per-block placeholders to the SVG's natural dimensions instead
    /// of the pre-render letterbox default. Coalesces a burst of
    /// MathRendered events into a single rebuild per frame.
    needs_md_reflow: bool,
    // ui/preview/editor: the annotation editor and file-parse staleness.
    /// `kernel.request file.parse` results, keyed by the relative path the
    /// kernel was asked about (matches the suffix of `files:` node ids).
    /// Used by the drift badge: if a row's path has a hash here AND its
    /// annotation parses a `synced_against`, and the two differ, yellow it.
    /// Lazy, per file, on view; a `preview.changed` for the file, a workspace
    /// switch and a reconnect drop it.
    file_ast_hashes: std::collections::HashMap<String, String>,
    /// Paths the GPU thread has already asked `file.parse` for. Prevents
    /// re-firing while a request is in flight, and a reply installs only while its
    /// path is latched. Cleared on a switch and on a reconnect.
    file_parse_fired: std::collections::HashSet<String>,
    /// Failed `file.parse` bookkeeping: path → (last failure, attempt
    /// count). The retry gate in `maybe_fire_concept_read` re-fires only
    /// after an exponential backoff and gives up after
    /// `FILE_PARSE_MAX_RETRIES`. WITHOUT this, a kernel that fails fast
    /// (dead / respawning) turned the unthrottled redraw loop into a
    /// request storm — observed ~4.7k file.parse/s on a capture box, which
    /// flooded the respawning kernel straight back down (2026-07-02).
    /// Cleared on success. Deliberately NOT in the workspace snapshot:
    /// a workspace switch resets the attempts, which is a feature.
    file_parse_retry: std::collections::HashMap<String, (std::time::Instant, u32)>,
    /// Active concept-annotation edit, or `None` for read-only view.
    /// Toggled by `e` (enter) / `Esc` (discard) in Preview focus.
    /// Save fires `concept.write` with the captured
    /// `expected_ast_hash` so the backend's stale-gate engages.
    edit_state: Option<EditState>,
    /// Node id (`files:<relpath>`) of a general-file edit-enter awaiting its
    /// `file.read` reply; the editor opens when a reply's node id matches,
    /// then this clears. `None` when not entering a file edit. Transient —
    /// not snapshotted.
    pending_file_edit: Option<String>,
    /// Shaped preview buffer for the active edit. Rebuilt eagerly on
    /// every key that mutates the edit buffer; rendered in place of
    /// the file-preview markdown when `edit_state` is Some. None when
    /// not in edit mode.
    preview_edit: Option<MarkdownPreview>,
    // ui/persist: settings.
    /// User-tunable chrome settings (layout proportions today, future
    /// general settings). Loaded once at startup via the same layered
    /// discovery as `bindings`; not re-read on file change.
    settings: Settings,
    // pages.rs (pages): page-proxy arming.
    /// ADR 0035 daemon TCP proxy — frontend half.
    /// `proxy_capable_hosts`: every host whose loopback pages this FE will
    /// proxy — inserted/removed per host from ITS OWN `Connected`/next
    /// `Connected` evt (`!matches!(resolved, ResolvedDial::Local) && proxy`),
    /// never FE-global: a box that also runs a local (pipe) daemon (ADR 0042)
    /// gets a `Connected{resolved: ResolvedDial::Local, ..}` from that one
    /// too, and a global flag let it silently switch proxying
    /// off for every OTHER host's pages (2026-09-10 field incident: every
    /// backend page "can't be reached" on a FE box with a local sotd, with
    /// nothing logged — the gate returned before the first log line). A later
    /// gate that scoped this to `default_host` alone fixed that incident but
    /// broke proxying for every non-default row's figures (cross-host figure
    /// defect) — per-host tracking fixes both: no host's evt can touch
    /// another's entry, and no host is structurally excluded. The actual
    /// dial for a proxy-capable host is read from `host_resolved_dial`
    /// (ADR 0045 decision 1) at arm time — never re-derived or restricted to
    /// one host — so `ensure_proxy_for_url` opens the proxy against the SAME
    /// daemon connection that owns the row, whichever host that is.
    /// `proxy_listener_tx`: hands GPU-thread-bound `std` listeners to the
    /// runtime accept loop, each tagged with its daemon port and the dial + token to
    /// reach that daemon through (`None` when no host has a
    /// runtime at all).
    /// `proxy_ensured`: per (host, daemon port), the port of the listener this
    /// frontend bound for it and the `Arm` that listener shares; opening a page
    /// on it re-arms it, since the daemon may have refused the port (`bad_port`) since.
    proxy_capable_hosts: std::collections::HashSet<HostKey>,
    proxy_listener_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::pages::PageListener>>,
    proxy_ensured: std::collections::HashMap<(HostKey, u16), (u16, std::sync::Arc<crate::pages::Arm>)>,
    // relaunch.rs (distribution): the relaunch watcher's flag.
    /// Set by the relaunch-watcher thread when the sentinel file
    /// (`%LOCALAPPDATA%\sot\relaunch.request`) appears: `0` = no request,
    /// `75` = plain relaunch, `76` = converge (self-update prelude + freshness
    /// pass re-run before respawn). The sentinel's content picks the code —
    /// see `relaunch_sentinel_path`. The window-event handler observes a
    /// nonzero value and exits with that code so the supervisor restages the
    /// freshly-built binary and respawns us with `--relaunched`. ADR 0017.
    relaunch_flag: Arc<std::sync::atomic::AtomicU8>,
}


impl State {















































































}













pub(crate) mod preview;
pub(crate) mod render;
use render::*;
use self::preview::concept::{clear_drift, drift_is_stale, install_parsed_hash, split_frontmatter, ConceptInfo, FILE_PARSE_MAX_RETRIES};
use self::preview::editor::state::EditState;
use self::preview::pane::{
    is_raster_preview_mime, preview_max_scroll, preview_scroll_target, resolve_preview_changed,
    resolve_previewed_path, SAMPLE_MARKDOWN,
};

#[cfg(all(test, feature = "test-window-progress"))]
#[path = "result_tests.rs"]
mod result_tests;

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) use app::tests::NativeProgressLedger;

#[cfg(all(test, feature = "test-window-close"))]
pub(crate) use app::native_exit_tests::run_native_window_close;
#[cfg(all(test, feature = "test-pane-timing"))]
pub(crate) use app::native_pane_tests::run_native_pane_timing;

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) fn run_native_window_progress() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => app::native_children::window_progress_parent(),
        [c, role] if c == "child" && role == "fixtures" => app::native_children::state_fixtures_child(),
        [c, role] if c == "child" && role == "workload" => app::tests::minimized_window_drains_events_for_ten_minutes(),
        _ => anyhow::bail!("window_minimized takes no argument, or `child fixtures` or `child workload`"),
    }
}
