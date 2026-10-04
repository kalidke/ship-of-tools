// gpu.rs — winit + wgpu surface lifecycle.
//
// Owns the winit Window, wgpu Surface/Device/Queue, and the text layer
// (text.rs). Drives a redraw on RedrawRequested: clears, then draws text on
// top in the same render pass.
//
// chrome.rs (ratatui custom Backend) and preview.rs (preview-layer surface)
// will plug in here as additional draw stages, both feeding into the same
// wgpu surface — see ADR 0011 for the chrome-vs-preview-layer split.

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

use crate::chrome::WgpuBackend;
use crate::edit_buffer::EditBuffer;
use crate::dial::HostKey;
use crate::keybindings::{Action, KeyBindings, Modifiers};
use crate::help;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use crate::preview::markdown::{
    FigureMetrics, FigureMetricsMap, MarkdownPreview, MathMetrics, MathMetricsMap,
    BODY_SIZE as MD_BODY_SIZE,
};
use crate::preview::png::quad_from_png_bytes;
use crate::preview::quad::{Quad, QuadPipeline, ScreenRect};
use crate::preview::svg::quad_from_svg_bytes;
use crate::preview::markdown::media::{
    parse_math_svg_dims, whole_row_bottom, MathSvg, TableBufferEntry, MATHJAX_EX_FACTOR,
};
use crate::settings::Settings;
use crate::transport::OutgoingReq;
use crate::net::hosts::{PendingTransport, lane_dial, resolve_default_host, resolve_monitor_host};
use crate::pages::{open_html_in_browser, open_url_in_browser};
use crate::lease::{ExitReason, ExitStep, close_now, exit_intent};
use crate::relaunch::relaunch_sentinel_path;
#[cfg(windows)]
use crate::relaunch::{allow_next_foreground, force_os_foreground};
use crate::net::identity::self_comm_handle;
pub(crate) use crate::net::identity::{FrontendIdentity, frontend_identity};
use sot_protocol::ops::LeaveIntent;
use sot_protocol::{ReplFrame, TreeNode};

mod app;
use app::*;
pub(crate) use app::App;
use preview::image::figures::{decode_figure_bytes, fail_figure, FigureCacheEntry};
#[cfg(test)]
use preview::image::overlay::CAPTION_MAX_CHARS;
use preview::image::overlay::{
    parse_physical_scale, truncate_caption, CaptionStore, PhysicalScale,
};
use preview::image::view::{
    image_rect_for_caption, letterbox, png_cache_key_from_node_id, png_zoom_max,
    preview_image_pane_px, roi_rects_within_quantization, solve_roi_view, visible_roi_px,
    PreviewRoi, RoiAim, RoiRect,
};
mod nav;
use nav::*;
pub(crate) use nav::files::download;

pub(crate) mod chrome;
use chrome::*;
pub(crate) mod drawer;
#[cfg(windows)]
use drawer::terminal::backend::drawer_uses_attach;
use drawer::repl::lines::{build_repl_lines, pinned_repl_scroll, ReplImage, ReplImageSlot};
use drawer::repl::log::ReplEntry;
use drawer::terminal::backend::scroll_drawer_ring;
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































use crate::text::TextLayer;












// ADR 0045 decision 1 (Codex review, lane B5 discharge); reshaped by C3 as
// amended (isolation-plan.md §3, dev/output/c3-second-connection-
// amendment.md §1): `ResolvedDial` — which transport a host's CONTROL
// connection actually resolved to — moved to `crate::transport`, beside
// `TransportConfig`, because `IncomingEvt::Connected` now carries it. See
// its doc there. No longer `Copy` (`SshRecipe` isn't); every former
// `.copied()` reader below is `.cloned()`.
use crate::transport::ResolvedDial;

mod connections;
mod page_proxy;
mod events;
mod init;



struct State {
    // ui/render: the wgpu surface and device, text layer, fixed quads, cell metrics and --capture state.
    window: Arc<Window>,
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
    evt_rx: std::sync::mpsc::Receiver<(crate::dial::HostKey, crate::transport::IncomingEvt)>,
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
        crate::dial::HostKey,
        tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
    )>,
    // net/hosts.rs (fe-net): the per-host connection table.
    /// Per-host connection status, derived from each connection's own
    /// `Connected`/`Disconnected`/`ProtocolMismatch` events (ADR 0042 L2a —
    /// no new wire signal). Absent or `false` = unreachable (never
    /// connected, or currently reconnecting); `true` = connected. The
    /// Sessions tree's host nodes read this to badge status and grey an
    /// unreachable host's (retained) workspace rows.
    host_connected: HashMap<crate::dial::HostKey, bool>,
    /// ADR 0045 decision 1: each host's own `TransportConfig` (its
    /// `lane.connect` bridge dial), so the session pane's capsule attach
    /// can reach THAT row's daemon — never a supervisor socket or a
    /// state-dir path directly. Filled once, from the same
    /// `PendingTransport` list `conns` is built from, before `resumed()`
    /// consumes it (`spawn_pane_attach_term` is the only reader).
    host_transports: HashMap<crate::dial::HostKey, crate::transport::TransportConfig>,
    /// ADR 0045 decision 1 (Codex review, lane B5 discharge): which
    /// transport each host's CONTROL connection actually resolved to
    /// (`ResolvedDial`'s own doc) — recorded from every `Connected` evt,
    /// consulted by `lane_dial`/`spawn_pane_attach_term` so the capsule
    /// lane dials the SAME endpoint, never a second independent guess.
    /// Absent for a host that hasn't connected yet.
    host_resolved_dial: HashMap<crate::dial::HostKey, ResolvedDial>,
    /// One link gate per host (`sot_protocol::ssh_bridge::LinkGate`): the
    /// host's control transport writes it, and every other site that starts
    /// an ssh login to the host (lane dials, the page proxy) asks it.
    /// Always taken through `entry().or_default()`, so there is exactly one.
    link_gates: HashMap<crate::dial::HostKey, sot_protocol::ssh_bridge::LinkGate>,
    /// ADR 0046 decision 1 (revised): the daemon's own declared identity
    /// for each dial — `HostKey` stays the stable dial label. Read by
    /// `host_label` (display: Hosts mode, Sessions labels, the status
    /// line, log lines) AND, since the session-listing brief, by the
    /// `Workspaces`/`Connected` event arms to decide whether a dial is
    /// the LOCAL daemon (its declared host equals `frontend_identity().host`)
    /// — the only thing that gates `fe.sessions`. Absent for a host that
    /// hasn't completed hello yet.
    declared_host: HashMap<crate::dial::HostKey, String>,
    /// F5 fires this to collapse the transport's exponential-backoff
    /// sleep and attempt an immediate reconnect — useful when wifi
    /// flickers and the user knows it's back before the current
    /// backoff cycle would have noticed. Held on State (not App) so
    /// the keyboard handler reaches it via &mut state.
    reconnect_now: Arc<tokio::sync::Notify>,
    // lease.rs (lifecycle): the window's leases and its leave.
    leases: Arc<crate::lease::Leases>,
    /// This window's own state root, as the daemon names it in a granted
    /// lease; the attach-only drawer needs a lease that carries it (ADR 0050).
    #[cfg(windows)]
    own_state_root: Option<String>,
    /// The not-ended line a lease grant reported once a frame showed it, and
    /// when it stops showing.
    not_ended_shown: Option<(String, std::time::Instant)>,
    /// Set once the window is on its way out and waiting for the daemons' acks.
    leaving: Option<crate::lease::Leaving>,
    // ui/chrome: the status line, flashes, focus, pane rects, maximize and nav spill.
    /// One-line status string for the chrome.
    status: String,
    /// While `Some(t)` and `now < t`, the status line is holding a pushed
    /// notify (op::FE_COMMAND `notify`) and `rebuild_connection_status` won't
    /// clobber it — so a toast survives a workspace switch for a few seconds
    /// instead of being overwritten instantly (which otherwise made a pushed
    /// notify un-seeable while the user roamed workspaces). Cleared + the
    /// status rebuilt once elapsed (in `about_to_wait`, on the idle tick).
    notify_sticky_until: Option<std::time::Instant>,
    /// `(host, slug)` → the `Instant` its work-state last changed, driving
    /// the status-change *flash* (the name brightens toward white then
    /// fades over `FLASH_SECS`). Entries are pruned once they age past the
    /// fade so the map stays small, and while any entry is live
    /// `about_to_wait` schedules a faster repaint so the fade animates.
    flash_starts: HashMap<WsKey, std::time::Instant>,
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
    /// Which pane has keyboard focus. Tree by default; Ctrl+Arrow moves
    /// focus. REPL focus consumes character keys as code rather than
    /// firing tree navigation.
    focus: PaneFocus,
    /// The four pane content rects from the most recent redraw, cached
    /// so keyboard handlers can size scroll steps to a real viewport
    /// (`PgUp/PgDn`, `Ctrl+u/d`). Updated at the end of every redraw.
    pane_rects: PaneRects,
    /// When `true`, the currently-focused pane fills the whole window;
    /// the other three are zero-sized and not drawn. Toggled with
    /// `Action::MaximizePane` / `Action::RestoreLayout` (defaults
    /// `Alt+=` / `Alt+-`). Maximisation tracks `focus` rather than
    /// being pinned to a specific pane — Ctrl+Arrow while
    /// maximised swaps which pane is on screen, which is what the user
    /// wants when they're staring at one pane and want to peek at
    /// another.
    maximized: bool,
    /// When `true`, the LLM column is hidden and its width handed to
    /// the preview pane (nav keeps its width) — the "wide preview"
    /// layout. Toggled with `Action::ToggleWidePreview` (default
    /// `Alt++`); Esc also exits it from the reading panes (after
    /// un-maximizing, and never over a pty or modal). Focus can't land
    /// on the hidden LLM pane while active; a capture-ROI paste that
    /// targets the LLM pane reveals it again.
    wide_preview: bool,
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
    /// Bottom drawer state — `Closed`, `Repl` (Ctrl+J), or `Terminal`
    /// (Ctrl+T). When open it takes its configured fraction of vertical
    /// space and the columns shrink. The variant selects which content
    /// renders; `layout::compute` only needs `drawer.is_open()`.
    drawer: DrawerContent,
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
    /// `keybindings.toml` if present). See `keybindings.rs` for the
    /// file format and discovery order. Read-only once loaded — the
    /// chrome doesn't reload mid-session.
    bindings: KeyBindings,
    help: help::Help,
    help_origin: Option<(PaneFocus, DrawerContent, bool)>,
    help_start_pending: bool,
    help_peek_start_pending: bool,
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
    /// Last `preview.get` we asked for, so cursor-move-driven refresh
    /// doesn't re-fire when the cursor is held on a row. Only files-mode
    /// rows generate a target (`files:` prefix); modules-mode rows have
    /// no backend-side preview.
    preview_node_id_fired: Option<String>,
    /// Blink guard for imperatively-driven previews (fe-command / nav.preview).
    /// When a driven preview targets a node NOT in the current tree rows (a deep
    /// path whose ancestors aren't expanded), the cursor stays on the OLD row,
    /// and `maybe_fire_preview` would otherwise fire that old row's preview right
    /// over the driven one — the deep-path blink. While this holds the old row's
    /// node id, `maybe_fire_preview` is suppressed for that exact row; the hold
    /// lifts the instant the cursor moves elsewhere. `None` normally. The
    /// deep-path cursor-reveal below (`pending_reveal`) supersedes this for
    /// same-workspace driven opens — there the cursor *does* follow the preview;
    /// the hold only bridges the brief window while ancestors are expanding.
    driven_preview_hold_cursor: Option<String>,
    /// Reconnect nav restore (2026-07-11): set on hello/reconnect resume so
    /// the NEXT applied tree.root re-reveals the pre-reconnect cursor (via
    /// the pending_reveal machinery — the cursor's ancestor path re-expands
    /// for free) instead of dumping the user at the top with everything
    /// collapsed. Windows Modern Standby makes this fire on every idle/wake
    /// cycle, so this is the difference between "nav keeps resetting" and
    /// continuity. Carries the workspace the flag was armed for — a
    /// workspace switch between hello and the root reply must NOT re-reveal
    /// the old workspace's path in the new tree.
    restore_nav_after_resume: Option<(Option<String>, String)>,
    /// (mode, tree.selected) snapshot from the previous redraw — used
    /// by the nav-fire debounce to detect cursor moves and stamp
    /// `cursor_moved_at`. Mode is included so a mode swap (which
    /// replaces the tree wholesale) is treated as a move.
    last_cursor_pos: Option<(Mode, usize)>,
    /// Wall-clock timestamp of the most recent cursor change in
    /// NavTree. Cleared after preview.get / concept.read / file.parse
    /// finally fire; while `Some`, those round-trips are suppressed
    /// (see [`NAV_FIRE_DEBOUNCE`]). Without this gate hold-to-scroll
    /// fires hundreds of backend requests per second.
    cursor_moved_at: Option<std::time::Instant>,
    /// C2 pin-and-leave: when `Some`, the preview pane stays on this
    /// node's rendered content even as the user moves the cursor away.
    /// `maybe_fire_preview` is a no-op while pinned. `p` (NavTree focus)
    /// toggles: pressing on a non-pinned cursor row pins it; pressing
    /// when the cursor is on the already-pinned row clears the pin.
    /// Per-workspace via [`WorkspaceUiSnapshot`].
    pinned_preview_node_id: Option<String>,
    /// Absolute path of the most recent `project.scan`'s `project_root`.
    /// The chrome strips this prefix off the absolute file paths each
    /// Modules-mode entry carries to synthesize the `files:<relpath>`
    /// node id that `preview.get` expects. Reset on each scan reply.
    scan_project_root: Option<String>,
    /// One-shot from `--start-selected <n>`; consumed by the first tree.root
    /// response that lands so the cursor opens on that row.
    /// `None` after consumption (or if the flag wasn't set).
    pending_initial_selection: Option<usize>,
    /// One-shot nav cursor restore across an ADR-0017 relaunch: the
    /// persisted `(selected node id, scroll)`. Applied best-effort when
    /// the matching workspace's `tree.root` arrives and the row is
    /// present; a deeply-collapsed selection that isn't in the freshly
    /// loaded tree just lands on the default cursor.
    pending_resume_nav: Option<(String, u16)>,
    /// Per-(mode, scope) parking lot for every nav tree that is NOT the
    /// active one (`self.tree` IS the active slot). Replaces the old
    /// `files_tree_workspace` provenance side-stamp + per-consumer
    /// staleness guards: installs route by the reply's key, so the active
    /// view can only ever hold the active (mode, workspace)'s tree — a
    /// foreign tree is structurally impossible rather than detected.
    tree_store: TreeStore,
    /// One-shot PER WORKSPACE: on a workspace's first Files-mode `tree.root`
    /// where nothing else (ADR-0017 resume, `--start-selected`) placed the
    /// cursor, default it to the project README so a fresh session opens
    /// onto rendered docs instead of the bare root row. Keys are `WsKey`
    /// (`active_ws_key()`, ADR 0042 L2a -- was bare `current_workspace_key()`,
    /// which let two hosts' same-slug workspaces share a one-shot marker);
    /// presence = already defaulted (or resumed) once, so refreshes never
    /// yank the cursor.
    nav_readme_defaulted: std::collections::HashSet<WsKey>,
    /// Persistent scroll offset for the nav pane (vim-style scrolloff
    /// behaviour). Updated each frame from the cursor's position relative
    /// to the current viewport: when the cursor moves into the bottom
    /// 1/3 of the pane going down, scroll keeps it stationary there;
    /// same on the way up. At the body's edges the cursor falls through
    /// to the actual top/bottom row. Kept on State so the cursor
    /// position alone doesn't determine the scroll — direction of motion
    /// matters.
    tree_scroll: u16,
    /// Switch-latency Phase 1: monotonic "latest issued" counter for the
    /// preview slot (`preview.get` + `preview.set_scale` share one slot —
    /// both install through the single `IncomingEvt::Preview` path).
    /// Incremented every time a request for either op is fired and stamped
    /// into the `OutgoingReq`/`PendingKind`/`IncomingEvt` trio unchanged;
    /// the reply handler drops any reply whose echoed generation is behind
    /// this counter's CURRENT value — a strictly-increasing counter means
    /// only the most recently fired request can ever match, so an earlier,
    /// slower reply (same workspace, a different node, a different host —
    /// even the daemon answering requests out of order) can never overwrite
    /// what a later cursor move already asked for. Paired with an owner
    /// check (host/workspace/node) as defense in depth for the one case
    /// generation alone can't see: `active_host`/`active_workspace_id`
    /// changing without firing a fresh preview request.
    preview_req_gen: u64,
    /// Same mechanism as `preview_req_gen`, for the concept/annotation
    /// slot (`concept.read`, cursor-tracking + stale-reload re-fire share
    /// this one counter — both land in the same `IncomingEvt::ConceptRead`
    /// consumer).
    concept_req_gen: u64,
    /// Same mechanism as `preview_req_gen`/`concept_req_gen`, for
    /// `project.scan` — keyed per (host, workspace) rather than one global
    /// counter, since scans for different workspaces are independently
    /// valid in flight together (see `next_project_scan_gen`).
    project_scan_req_gen: HashMap<(HostKey, Option<String>), u64>,
    // ui/nav/files: nav prompts, the reveal walk and file transfers.
    /// Active NavTree modal prompt (Ctrl+N new-file, future delete-confirm),
    /// or `None` in the normal nav state. While `Some`, the NavTree key
    /// handler routes keystrokes into the prompt before any nav shortcut and
    /// the chrome shows the prompt on the status line.
    nav_prompt: Option<NavPrompt>,
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
    /// Focus to restore when the scale-entry prompt resolves (confirm OR
    /// cancel). Ctrl+S fires from the Preview pane, and `begin_scale_entry`
    /// takes NavTree focus purely because that's where NavPrompt keystrokes
    /// are handled — an implementation detail, not something the user asked
    /// for. Without restoring, calibrating an image you're inspecting dumps
    /// you in the tree, so your next zoom/pan keypress goes to the wrong pane.
    /// `None` when no scale prompt is open.
    scale_entry_prior_focus: Option<PaneFocus>,
    /// One-shot cursor reveal for a preview driven via a *workspace switch*
    /// (#4 fix). Cross-ws force-show previews and #1's persisted `nav.preview`
    /// are consumed in `switch_to_workspace`, which fires the preview body
    /// before the switched-to workspace's `tree.root` has loaded — so the
    /// target row isn't in `tree.rows` yet and the cursor can't land inline.
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
    /// ADR 0014 active workspace. `None` resolves to the daemon's
    /// default workspace (the project the backend was launched with).
    /// `Some(slug)` routes tree/preview ops through the corresponding
    /// workspace's FilesMode + Kernel. Set when the user Enters a row
    /// in Sessions mode and persisted to `state-<hostname>.toml` so a
    /// restart resumes in the same workspace.
    ///
    /// ADR 0042 L2a: bare, unchanged type — the pair `(active_host,
    /// active_workspace_id)` is what names the current workspace now that
    /// there's more than one connection.
    active_workspace_id: Option<String>,
    /// The union this refactor is built on (ADR 0042 L2a): host → its most
    /// recent `workspace.list` reply. A reply from host H replaces only
    /// H's entry — every other host's last-known list is untouched, so an
    /// unreachable host keeps showing its (greyed) rows instead of
    /// vanishing. `rebuild_workspace_caches` and the Sessions tree are
    /// both derived from this in `conns` order (see `ordered_hosts`), not
    /// insertion order.
    workspace_lists: HashMap<crate::dial::HostKey, Vec<crate::transport::WorkspaceInfo>>,
    /// Two-press confirm for `D` (workspace destroy) in Sessions mode.
    /// First press arms with the cursor row's `(host, workspace_id)`;
    /// second press on the same row fires `workspace.destroy` via
    /// `send_to` (a destroy targets a specific row, not necessarily the
    /// active one). Cleared by any other keypress (snapshot-then-reset at
    /// the top of the input handler) so a cursor move disarms the trap.
    pending_destroy_target: Option<WsKey>,
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
    /// when `active_workspace_id` is None (default workspace) and
    /// `workspace_labels` hasn't been populated yet (no `workspace.list`
    /// reply landed). Once workspaces arrive, the slug-keyed label wins.
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
    /// `(host, slug)` → label map populated from each host's own
    /// `workspace.list` reply. Used by `rebuild_connection_status` to show
    /// a friendly workspace name in the chrome status (e.g. "Alpha")
    /// rather than the raw slug. ADR 0042 L2a: keyed by `WsKey`, not a bare
    /// slug — two hosts each having a "sot" workspace must not collide.
    workspace_labels: HashMap<WsKey, String>,
    /// `(host, slug)` → project_root map populated from each host's own
    /// `workspace.list` reply. Used to resolve `files:<rel>` node ids to
    /// absolute paths in the *active* workspace (not the daemon's startup
    /// workspace). Falls back to `daemon_project_root` if the active
    /// workspace hasn't appeared in a `workspace.list` reply yet.
    workspace_project_roots: HashMap<WsKey, String>,
    /// `(host, slug)` → (agent_state, agent_status_at) from each host's own
    /// `workspace.list` reply, so the bottom session strip can colour each
    /// name by its agent's work-state (the same data the Sessions-mode rows
    /// carry in their node payload). Refreshes live on the daemon's
    /// registry-watch `workspace.changed` push; empty entries render with
    /// the default strip styling.
    workspace_states: HashMap<WsKey, (String, String)>,
    /// `(host, slug)` → REPL child lifecycle ("not_started" | "starting" |
    /// "ready" | "dead"), fed by BOTH sources: live `lifecycle` repl.frame
    /// evts (the supervisor announces spawn/first-line/death) and
    /// `workspace.list` replies (`repl_state`, the reconnect catch-up). The
    /// point is the *starting* state: the first REPL per workspace
    /// precompiles its project env (per-package env, #44) — minutes with
    /// zero output frames, previously indistinguishable from a dead kernel.
    /// NOT cleared on `workspace.list` — an old daemon sends empty
    /// `repl_state`, and frame-driven entries must survive it.
    repl_lifecycle: HashMap<WsKey, String>,
    /// `(host, canonical workspace_id)` → `(host, slug)`, from each host's
    /// own `workspace.list` reply. Lifecycle frames stamp the CANONICAL id
    /// (`ws-<slug>-<hash>`) — the Repl supervisor's identity — while every
    /// FE surface keys by `WsKey` (the `started`-frame lesson, 2026-07-16:
    /// a canonical-id compare against slug keys silently never matches).
    /// This map is the translation at the frame boundary.
    /// ADR 0042 L2a codex review, item J: the OUTER key is now
    /// host-qualified — a canonical id is `slug+pid+time`, but a LEGACY
    /// id is just the bare slug, so two hosts' same-slug legacy ids
    /// collided on a bare-id key (whichever host inserted last silently
    /// won, and a lookup from the OTHER host's frame resolved to the
    /// wrong slug). Looked up via the frame's own `event_host`
    /// (`lifecycle_key_of`'s `host` param), never trusted from the
    /// entry's own value.
    workspace_id_slugs: HashMap<(HostKey, String), WsKey>,
    /// Badge floor (ADR 0025 §1): `WsKey` (host, slug) → workspace-relative
    /// path of a `nav.preview` result that arrived for a workspace the FE
    /// wasn't viewing. Instead of silently dropping the off-workspace
    /// result (the bug §1 fixes — a backend session pushes a result to a FE
    /// looking at another workspace and it vanishes), we record it here
    /// ("result pending") and badge that workspace's row/strip name
    /// non-disruptively. Latest-wins per workspace. When the user later
    /// *switches* to the workspace, `switch_to_workspace` drives the
    /// pending preview and clears the entry, so a result always reaches
    /// the user — never dropped. `dispatch_fe_command`'s `Preview`/`Reveal`
    /// arms reuse `mark_pending_nav` too.
    /// ADR 0042 L2a codex review, item E: keyed by `WsKey`, not a bare
    /// slug. The `nav.preview` ENVELOPE itself carries no host field, but
    /// the EVENT delivering it does (`event_host`, tagged by whichever
    /// connection received the `agent.message` push) — the same slug on
    /// two hosts is exactly the collision `WsKey` exists to prevent
    /// everywhere else, and a bare-slug compare here let a same-slug
    /// nav.preview from a NON-active host be mistaken for one targeting
    /// our active workspace.
    pending_nav: HashMap<WsKey, String>,
    /// `(host, slug)` → the *previous* work-state string we last saw, so
    /// the `workspace_states` update site can tell a real transition (a
    /// slug that had a known, different prior state) from a first-ever
    /// appearance. Only real transitions flash; a slug showing up for the
    /// first time does not.
    prev_workspace_states: HashMap<WsKey, String>,
    /// Ordered list of `(host, slug)` from the most recent per-host
    /// `workspace.list` replies (union across every connected host, each
    /// host's own slugs alphabetical). Drives the Shift+ArrowLeft /
    /// Shift+ArrowRight cycle hotkey (D7) — the "next" workspace is the
    /// next entry in this vec, wrapping at both ends. Empty until the
    /// first reply lands.
    workspace_slugs: Vec<WsKey>,
    /// Bare slug of `active_host`'s workspace flagged `is_default` in its
    /// `workspace.list`. Used to interpret `active_workspace_id == None`
    /// as "we're on the default workspace". Deliberately NOT a `WsKey`
    /// (unlike its sibling caches): every consumer (`ws_key_of`,
    /// `TreeScope::Workspace`, `eval_id_workspace`, the UI/REPL snapshot
    /// maps) is workspace-cache bookkeeping that predates hosts entirely
    /// and stays scoped to "the active host's default slug" — out of ADR
    /// 0042 L2a's named list, and retyping it would ripple into that
    /// whole layer for no invariant this slice needs. `None` until the
    /// reply lands.
    default_workspace_slug: Option<String>,
    /// Sessions-mode workspace picker (ADR 0014). `Some(state)` while
    /// the user is browsing a directory tree to pick the project_root
    /// of a new workspace; `None` outside the picker. Supersedes the
    /// older label-only prompt that fired `tmux.create_session` with
    /// a hardcoded `$SOT_PROJECTS_ROOT/<label>` cwd.
    workspace_picker: Option<WorkspacePicker>,
    /// Per-workspace NavTree snapshots. Captured when the user
    /// switches away from a workspace; restored when they switch back.
    /// Keyed by `WsKey` (ADR 0042 L2a, Codex review PR #163: bare-slug
    /// keying let "sot" on host A and "sot" on host B share a slot --
    /// `"<default>"` for the daemon-default within a host). Means
    /// switching workspaces (or hosts) doesn't lose cursor position or
    /// the expanded-folder shape of the file tree.
    workspace_ui_snapshots: HashMap<WsKey, WorkspaceUiSnapshot>,
    /// Per-workspace REPL snapshots. Captured separately from the UI
    /// snapshot because `ReplEvalDone` replies can land for workspaces
    /// the user has swapped away from, and those need to mutate the
    /// owning workspace's log directly. Keyed the same way (`WsKey`,
    /// `"<default>"` for the daemon-default workspace within a host).
    workspace_repl_snapshots: HashMap<WsKey, WorkspaceReplSnapshot>,
    /// The last `fe.sessions` projection this frontend sent out
    /// (session-listing brief decision 2) — `None` until the local
    /// daemon's first `workspace.list` reply. Compared against the next
    /// projection so a declaration is re-sent only when it actually
    /// changed (edge-driven, no timer); also what a reconnecting hub is
    /// resent verbatim in the `Connected` arm, since its own fresh
    /// connection remembers nothing from before.
    last_declared_sessions: Option<Vec<sot_protocol::DeclaredSession>>,
    /// The connection every "current view" operation targets — cursor
    /// state, the active tree, `active_workspace_id`. The pair
    /// `(active_host, active_workspace_id)` names the current workspace
    /// (ADR 0042 L2a). Defaults to the first connection in `conns`
    /// (local-first, then --dial argument order — see `dial::resolve_connections`).
    active_host: crate::dial::HostKey,
    /// Pending 10 s read mark for the row a person just switched to.
    read_mark: Option<ReadMark>,
    /// LU6a design-review amendment: stamped by `commit_workspace_create`
    /// the moment `workspace.create` is SENT, and consumed (taken) by the
    /// `attach_session_to_bl` that `switch_to_workspace` always calls once
    /// the `WorkspaceCreated` reply lands — the handoff that makes a
    /// create's `since_request_ms` start at the create request rather
    /// than at the later switch. `None` whenever no create is in flight.
    pending_capsule_create_requested_at: Option<std::time::Instant>,
    /// Last time this FE sent `fe.presence` (2026-09-08 review rework,
    /// design point A) — `None` until the first real keyboard/mouse event.
    /// `window_event`'s `KeyboardInput`/`MouseInput` arms fan a send out to
    /// EVERY connected host's daemon (`report_presence`) whenever this is
    /// stale by more than `PRESENCE_THROTTLE` — ONE throttle gates the
    /// whole fan-out, not one per host, so a burst of input still costs at
    /// most one round of requests per window; idle input sends nothing at
    /// all (no timer, no heartbeat — presence is purely a side-effect of
    /// real events already being handled).
    presence_last_sent: Option<std::time::Instant>,
    // ui/drawer: the monitor drawer.
    /// Ctrl+M server-monitor drawer (ADR 0020): per-host metric ring + SVG
    /// chart renderer. `monitor_quad` is the rasterised chart painted into
    /// the drawer rect via the resvg→wgpu-quad path (same as MathJax);
    /// `monitor_rect_px` is the drawer's pixel rect from the last layout
    /// pass; `monitor_dirty` requests a re-render when data or size changes.
    monitor_view: crate::monitor_view::MonitorView,
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
    // ui/drawer/repl: the REPL drawer's log, input, figures and history.
    /// Inline REPL figures: decoded image frames keyed by
    /// (eval_id, frame index within the entry). Built in the pre-draw pass
    /// (never inside the draw closure — texture upload must not contend
    /// with the text layer borrow); pruned when the entry ages out of
    /// `repl_log`.
    repl_images: std::collections::HashMap<(u64, usize), ReplImage>,
    /// Row-slots the current frame's `build_repl_lines` reserved for
    /// inline images: absolute line index of the first reserved row, row
    /// count, display size, cache key. Rebuilt every draw.
    repl_image_slots: Vec<ReplImageSlot>,
    /// Scrollback sub-rect (px) + visible [start, end) line window of
    /// the last REPL drawer draw — the coordinate frame the image quads
    /// paint into.
    repl_scrollback_px: ScreenRect,
    repl_window: (usize, usize),
    /// What the last REPL line-build produced, for `pinned_repl_scroll`:
    /// the geometry it was laid out at (the raw bits of all four inputs the
    /// build consumes, so an unchanged `f32` compares equal), the newest
    /// entry's `eval_id`, and the span from that entry's first line to the
    /// end of the build. Keyed on the whole geometry because the line count
    /// moves with pane HEIGHT and cell metrics too, not just width — a
    /// taller window reserves more rows for a figure, and a font-scale step
    /// re-wraps — and compensating for that would jump the view. `None` =
    /// nothing to measure against (first draw, empty log, or a workspace
    /// swap replaced the log wholesale).
    repl_build_anchor: Option<((u32, u32, u32, u32), u64, usize)>,
    /// Tracks which host+workspace each in-flight eval belongs to. Set at
    /// `submit_repl_input` time using the live `active_host`/
    /// `active_workspace_id`; consumed by `ReplEvalDone`/`ReplFrameStreamed`/
    /// `ReplRunFileDone` so a reply routes to the right workspace's log even
    /// if the user has swapped away. Keyed by `(HostKey, eval_id)` (ADR 0042
    /// L2a, Codex review PR #163): each host's daemon independently assigns
    /// eval ids from its own counter (confirmed on the backend side --
    /// `EXEC_EVAL_ID` is a per-process static in rust/backend/src/handlers.rs
    /// -- and the FE's own `repl_eval_counter` resets per workspace-key too),
    /// so a bare `eval_id` collides the moment two hosts both have an eval
    /// id 1 in flight. The VALUE stays a `WsKey`, not a bare slug, for the
    /// same reason every other cache in this file moved off bare slugs.
    eval_id_workspace: HashMap<(HostKey, u64), WsKey>,
    /// Per-eval display info for an in-flight streaming `repl.run_file`,
    /// stashed when the *acceptance* ack lands (it carries the resolved
    /// basename/project/fresh but a 0 elapsed — the run hasn't happened yet)
    /// and consumed when the streamed `Done` frame finalizes, so the
    /// completion status line shows the real elapsed. Keyed by `(HostKey,
    /// eval_id)` -- same collision reasoning as `eval_id_workspace` (two
    /// hosts' daemons independently assign eval ids). Value:
    /// `(basename, project_dir, fresh)`.
    repl_runfile_status: HashMap<(HostKey, u64), (String, Option<String>, bool)>,
    /// Scrollback for the REPL pane — appended on Enter (in-flight entry)
    /// and reconciled when the `ReplEvalDone` event lands. Bounded to the
    /// last few hundred entries by a simple cap at drain time so a long
    /// session doesn't grow unbounded.
    repl_log: Vec<ReplEntry>,
    /// Current single-line input buffer for the REPL pane. Submitted on
    /// Enter in REPL focus; cleared after send. Multi-line input is a
    /// follow-up.
    repl_input: String,
    /// Per-session eval id counter. The wire's `eval_id` is what lets us
    /// find the in-flight entry when the response lands; the counter
    /// is local to the chrome.
    repl_eval_counter: u64,
    /// Scroll offset (rows from the tail) of the REPL pane. 0 = live,
    /// positive = looking at older lines. Reset to 0 whenever the user
    /// types into the REPL so typing always snaps to live; otherwise
    /// updated by the mouse wheel when the cursor is over the BR pane.
    /// Clamped to [0, total_lines - viewport_h] at render time.
    repl_scroll: u16,
    /// REPL prompt mode. `false` = `julia>` (default), `true` = `pkg>`.
    /// User toggles via `]` at start of empty input (enter) /
    /// `Backspace` at start of empty input in pkg mode (leave). When
    /// true, `repl.eval` requests carry `mode: "pkg"` so the backend
    /// routes through `Pkg.REPLMode.do_cmds` (`b07b4f0`).
    repl_pkg_mode: bool,
    /// REPL history walk position. `None` = not walking; the user is
    /// editing a fresh buffer. `Some(p)` indexes into the filtered list
    /// of completed `repl_log` entries (oldest = 0, newest = len-1).
    /// Set by ArrowUp/ArrowDown; cleared by submit. Edits to
    /// `repl_input` while walking don't clear it — next Up/Down still
    /// walks from the same position, matching standard shell behaviour
    /// (the edit is lost, not preserved across the next history step).
    history_pos: Option<usize>,
    /// In-progress buffer saved on the first ArrowUp of a history walk.
    /// Restored when Down walks past the newest entry. `None` outside
    /// of a walk (mirrors `history_pos`).
    history_saved: Option<String>,
    // ui/drawer/terminal: the terminal drawer's backends.
    /// Local PTY terminal hosting the OS shell (G2). Lazily spawned the
    /// first time the Terminal drawer opens (Ctrl+T); a separate field
    /// from the ratatui `terminal` so the draw closure's `self.terminal`
    /// borrow and this terminal's `screen()` borrow are disjoint.
    local_term: Option<crate::term::LocalTerminal>,
    /// ADR 0041 step 6 U3: the attach-only alternative to `local_term`,
    /// live only when `settings.attach_only` is on (Windows only — see
    /// `sot_log::fe_client_io`). Mutually exclusive with `local_term`:
    /// the Terminal drawer's lazy-spawn site picks exactly one backend
    /// at creation time and never both. `None` on every non-Windows
    /// build target (the field itself still exists there so the rest of
    /// this struct's layout doesn't fork by platform) since attach-only
    /// has nothing to attach to off Windows.
    #[cfg(windows)]
    attach_term: Option<sot_log::fe_client_io::FeAttachClient>,
    /// Last `(cols, rows)` the local terminal's PTY was sized to. `None`
    /// until the drawer rect is first observed; drives resize-on-change
    /// (mirrors `pty_size` for the LLM pane).
    term_size: Option<(u16, u16)>,
    /// Local repo root (`$SOT_REPO_DIR`, set by the supervisor). Used as
    /// the Terminal drawer's working directory, so the plain shell it spawns
    /// starts in the project root. `None` when launched outside the
    /// supervisor (then the shell inherits the frontend's cwd). ADR 0017.
    repo_dir: Option<std::path::PathBuf>,
    // ui/agent_pane: the agent pane's target, attach clients and selection.
    /// Owning host + tmux session the BL pane is attached to (ADR 0042
    /// L2a codex review, item D). `None` until the first `pty.open`
    /// reply lands; defaults to `sot-llm` semantically. Sessions-mode
    /// Enter (B3) updates this to the targeted backend session and
    /// fires a fresh `pty.open` with the new target so the backend
    /// kills + respawns the pty. MUST carry the host: two hosts can
    /// both name a session "sot-be-sot", and a bare session name used
    /// to make `attach_session_to_bl`'s "already attached" early
    /// return fire on a same-named session on a DIFFERENT host,
    /// leaving the pty open on the OLD host while every subsequent
    /// write/resize/scroll routed (via `active_host`) to the new one.
    bl_pane_target: Option<(HostKey, String)>,
    /// Last (cols, rows) sent on the session pane's `pty.open`, and the
    /// size a fresh `blank_pane_screen` draws at when nothing else is
    /// live. `None` = no `pty.open` sent yet; first BL redraw with a
    /// real rect fires the open.
    pty_size: Option<(u16, u16)>,
    /// Mouse selection in the LLM pane, as inclusive cell endpoints
    /// `(row, col)` in pane-relative coords. Both stored unnormalised
    /// (start = where the user pressed, end = follows the mouse);
    /// the copy + render paths reorder before walking. `None` = no
    /// selection.
    llm_selection: Option<((u16, u16), (u16, u16))>,
    /// True while the left mouse button is held down inside the LLM
    /// pane. Drag-motion CursorMoved events update `llm_selection.end`
    /// while this is set.
    llm_drag_active: bool,
    /// ADR 0042 slice L1b: the session (BL) pane's OWN attach client —
    /// live when the selected workspace row's cached `runtime ==
    /// "capsule"`. A SEPARATE client from `attach_term`: the drawer and
    /// the session pane are different terminal surfaces with different
    /// tenants (ADR 0041 "one drawer, tenant fixed" vs every workspace
    /// being an ordinary peer session, ADR 0042). Set/cleared by
    /// `attach_session_to_bl` (dropped on every selection change) and the
    /// `PtyAttachDirect` handler (the only place it is ever spawned, ADR
    /// 0042 shrink round rule A). ADR 0045 decision 1: attaches through
    /// the row's own daemon (`DaemonLaneEndpoint`) on every platform, not
    /// only Windows — `None` whenever the selected row is a tmux
    /// workspace.
    pane_attach_term: Option<PaneAttachClient>,
    warm_attach: WarmAttachPool<PaneAttachClient>,
    /// LU6a: the pane's last content, held across a capsule switch until
    /// the new client's checkpoint lands — see `HeldPaneScreen`'s own doc
    /// and `pane_screen_choice`. Captured by `attach_session_to_bl` when
    /// dropping the departing feed; cleared the first time the new
    /// client reports checkpointed, when the attach itself fails
    /// (`spawn_pane_attach_term` returning false), or — coordinator
    /// amendment — when the client goes terminal (`is_dead`) before it
    /// EVER checkpointed (`pump_pane_attach_term`, alongside the episode-
    /// failure warn line): a dead end is not a stall, and must not keep
    /// showing the departed row's screen forever. Never left to outlive
    /// the switch it belongs to.
    pane_hold: Option<HeldPaneScreen>,
    /// SHOULD-FIX (Codex review, lane B5 discharge): a genuine dial
    /// CONFIGURATION error for the CURRENT `bl_pane_target`'s host —
    /// `spawn_pane_attach_term` sets this only when the host has no
    /// transport at all, or its resolved transport yields no usable
    /// dial; never for "not yet connected" (transient, no persistent
    /// reason set, ordinary retry stays enabled). Persistent and
    /// terminal: rendered with priority in the pane (see
    /// `pane_shows_terminal_reason`'s doc) and gates the daemon-
    /// reconnect handler's `pty.open` re-fire (`Some` suppresses it — a
    /// known-broken host must not be retried on every reconnect).
    /// Cleared on every switch (`attach_session_to_bl`) and on a later
    /// successful spawn for the same row.
    pane_dial_error: Option<String>,
    /// LU6a design-review amendment: when the SWITCH or CREATE that led
    /// to the live `pane_attach_term` was REQUESTED — the one frontend
    /// clock the attach-outcome log lines (`pump_pane_attach_term`)
    /// report `since_request_ms` against (the acceptance metric: total
    /// user-perceived latency, not merely "since the client object was
    /// constructed"). Stamped at `attach_session_to_bl`'s entry for an
    /// ordinary switch; for a workspace CREATE, inherited from
    /// `pending_capsule_create_requested_at` instead, so the daemon round
    /// trip to actually create the workspace counts too. `None` before
    /// the very first switch.
    pane_attach_requested_at: Option<std::time::Instant>,
    /// LU6a: when `spawn_pane_attach_term` last installed the live
    /// `pane_attach_term` — a SECOND, narrower clock the attach-outcome
    /// log lines report as `since_client_ms` (how long the client itself
    /// has been running), alongside `since_request_ms` above. `None`
    /// before the very first spawn.
    pane_attach_started_at: Option<std::time::Instant>,
    /// LU6a: how many non-success status changes `pump_pane_attach_term`
    /// has observed from the CURRENT `pane_attach_term` since it was
    /// installed — the "episode count" its warn-level attach-outcome log
    /// line reports (the observability gap this lane closes: the FE
    /// otherwise only ever logged "attaching"). Reset to 0 on every new
    /// spawn.
    pane_attach_episode_warnings: u32,
    /// Switch-latency Phase 1, item 3: one-shot per attach — set the first
    /// time the render loop actually paints THIS client's own screen
    /// (`pane_screen_choice` resolving to `PaneScreen::Client`), so the
    /// `capsule screen presented` line fires exactly once per attach, the
    /// same edge-triggered pattern `pump_pane_attach_term` already uses
    /// for "checkpoint applied"/"attached". This is the acceptance metric
    /// itself (keypress → current screen visible) — `checkpoint applied`
    /// only proves the client's OWN parser is ready, not that a frame with
    /// it on screen has actually been submitted for display. Reset to
    /// `false` on every new spawn (`spawn_pane_attach_term`).
    pane_attach_presented: bool,
    /// ADR 0042 slice L1b fix 2: which state the session pane's input
    /// routes to right now — see `PaneFeed`'s own doc for why this can't
    /// just be derived from `pane_attach_term.is_some()`. Starts
    /// `Pending`: the very first attach is exactly as unresolved as any
    /// later switch.
    pane_feed: PaneFeed,
    /// Session-pane input discarded before the pane could take it: typed
    /// while `pane_feed == PaneFeed::Pending`, or with no live client.
    /// Never sent later. Cleared at the client's "attached" edge
    /// (`pump_pane_attach_term`) and on a switch or reset of the row; the
    /// pane's top line shows it plus the client's own count.
    pane_inputs_discarded: usize,
    // ui/preview: the preview pane's buffers, concept slot, scroll and overlays.
    preview_svg: Option<Quad>,
    /// Tree-sitter-backed syntax highlighter for the markdown preview's
    /// fenced code blocks (and, later, the editor pane). Constructed
    /// once per `State` because `HighlightConfiguration::new` compiles
    /// the per-language highlight query — moderately expensive vs
    /// the per-redraw highlight call itself.
    highlight_service: crate::preview::highlight::HighlightService,
    preview_md: MarkdownPreview,
    /// Pixel rect of the markdown pane from the most recent ratatui layout
    /// pass; cached so we can re-shape on resize without re-running layout.
    md_rect_px: ScreenRect,
    /// Annotation state for the most-recently-fired `concept.read`. `target`
    /// is what we asked for; when the response arrives with the same target,
    /// `exists` and `content` are filled. Mismatch (cursor moved before the
    /// reply landed) is dropped — the chrome stays on the last good answer
    /// until the new one arrives, which keeps the status line from flickering.
    concept_target_fired: Option<String>,
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
    protocol_mismatch: HashMap<HostKey, String>,
    preview_fatal: Option<MarkdownPreview>,
    /// Last preview source (mime + raw bytes) cached so a font-size
    /// change can rebuild the preview at the new scale without a
    /// round-trip back to the backend. Cleared on disconnect.
    preview_src: Option<(String, Vec<u8>)>,
    /// The node id of the reply that installed `preview_src` — see the
    /// `WorkspaceUiSnapshot` field of the same name for why this is a
    /// distinct field from `preview_node_id_fired`.
    preview_src_node_id: Option<String>,
    // ui/preview/image: the image view, ROI, pages, scalebar, captions and figures.
    /// Spike-step-4 placeholders — kernel-driven previews replace these once
    /// transport lands.
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
    /// Host `capture_roi` fired its `image.crop` request against, pinned
    /// at send time (ADR 0042 L2a codex review, item F). The
    /// `ImageCropped`/`ImageCropFailed` reply is a multi-step op just
    /// like an upload — the reply pastes into (or fails to paste into)
    /// the LLM pane, and a workspace/host switch between the request and
    /// the reply landing must not let a stray reply from a NON-owning
    /// host drive that side effect. One-shot: consumed (`.take()`) by
    /// whichever of the two replies lands first.
    pending_roi_capture_host: Option<HostKey>,
    /// ADR 0025 `preview --roi`: an armed viewport aim awaiting its image
    /// (see `RoiAim`). Deliberately NOT per-workspace snapshot state — a
    /// cross-workspace aim must survive the switch that consumes its badge.
    pending_roi_aim: Option<RoiAim>,
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
    /// `(page, zoom)` of an in-flight zoom re-raster. The matching reply
    /// keeps the current zoom/pan (only the texture detail changes); a
    /// reply for any other page is a real navigation and resets the view.
    preview_page_raster_pending: Option<(u32, f32)>,
    /// One-shot: the next `image/png` render is a zoom re-raster of the
    /// page already on screen, so the quad swap must preserve zoom/pan
    /// rather than reset to fit. Set by the Preview handler, consumed in
    /// `render_preview_source`.
    preview_reraster_keep_view: bool,
    /// Physical scale of the raster the pane is *showing* (ADR 0034), from
    /// the reply's `extras.physical_scale`. `None` for previews with no
    /// scale; cleared on every `preview.get` reply (like `preview_page`).
    /// Presence gates the scalebar toggle key + overlay.
    preview_scale: Option<PhysicalScale>,
    /// One-shot: a `preview.set_scale` is in flight, as `(node_id, typed
    /// value)`. The success reply deliberately flows through the SHARED
    /// preview handler (one install path for a preview and its calibration),
    /// and that handler knows nothing about scale entry — so without this the
    /// "saving…" status would sit there forever even though the bar had
    /// already appeared. Taken by whichever resolves first: the preview
    /// install (success) or `ScaleSetFailed` (rejection), so a stale marker
    /// can't make some later unrelated preview claim "saved".
    ///
    /// The `node_id` is carried so the take() can require the arriving preview
    /// to BE the save's target: navigating to another file mid-save would
    /// otherwise let that file's `preview.get` consume the marker and announce
    /// "saved" over an unrelated image. Cosmetic — the sidecar and bar are
    /// correct either way — but a status line attributing a save to the wrong
    /// file is exactly the kind of quietly-wrong message worth not shipping.
    scale_save_pending: Option<(String, String)>,
    /// Scalebar overlay toggle (ADR 0034, `b`). Default off; sticky across
    /// navigations (renders only when `preview_scale` is present, so it can
    /// stay armed while browsing a mix of scaled / unscaled rasters).
    scalebar_on: bool,
    /// The shaped scalebar label buffer (e.g. `500 nm`), rebuilt each frame
    /// from the adaptive bar length by `build_scalebar` (shaped OUTSIDE the
    /// render pass). `None` when the bar isn't drawn.
    scalebar_label: Option<crate::preview::markdown::MarkdownPreview>,
    /// Agent-supplied figure captions, sticky per (workspace, file). Written by
    /// the `preview`/`reveal` fe-commands, read at render time for whichever
    /// image the pane is actually showing. Deliberately NOT part of
    /// `WorkspaceUiSnapshot`: the key already carries the workspace, so unlike
    /// `preview_scale` there is no swap-in path that could hand one workspace's
    /// caption to another's image.
    preview_captions: CaptionStore,
    /// The shaped caption buffer for the image on screen, rebuilt each frame by
    /// `build_caption` (shaped OUTSIDE the render pass, like `scalebar_label`).
    /// `None` when no caption is drawn.
    caption_label: Option<crate::preview::markdown::MarkdownPreview>,
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
        std::collections::HashMap<(String, u64), Vec<crate::transport::MarkdownToken>>,
    /// In-flight markdown.tokenize requests, same key shape as
    /// `markdown_token_cache`. Prevents duplicate dispatch when the
    /// same fence appears twice in a doc or the user re-renders before
    /// the reply lands.
    markdown_token_pending: std::collections::HashSet<(String, u64)>,
    /// Node id of the markdown file backing the current `preview_md`
    /// — used to resolve relative `![](url)` paths against the
    /// markdown's own directory. Set whenever a `text/markdown`
    /// preview lands carrying a node id; `preview_node_id_fired` is
    /// not authoritative here because the `--capture-preview` path
    /// deliberately pins it to the root row to suppress
    /// cursor-driven re-fires.
    current_md_node_id: Option<String>,
    /// Workspace the current markdown was fetched from. Figure
    /// fetches use this rather than `active_workspace_id` so a
    /// session whose active workspace differs from the markdown's
    /// (e.g. `--capture-preview` always uses default) still routes
    /// the figure to the right project.
    current_md_workspace_id: Option<String>,
    /// Set by the MathRendered event handler when a fresh SVG enters
    /// `math_cache`. The redraw loop checks this and rebuilds
    /// `preview_md` from `preview_src` so the markdown walk can size
    /// per-block placeholders to the SVG's natural dimensions instead
    /// of the pre-render letterbox default. Coalesces a burst of
    /// MathRendered events into a single rebuild per frame.
    needs_md_reflow: bool,
    // ui/preview/editor: the annotation editor and file-parse staleness.
    /// `kernel.request file.parse` results, keyed by the relative path the
    /// kernel was asked about (matches the suffix of `files:` node ids).
    /// Used by the drift badge: if a row's path has a hash here AND its
    /// annotation parses a `synced_against`, and the two differ, yellow it.
    /// Grows as the user navigates — phase-2 may sweep eagerly.
    file_ast_hashes: std::collections::HashMap<String, String>,
    /// Paths the GPU thread has already asked `file.parse` for. Prevents
    /// re-firing while a request is in flight. Cleared on disconnect.
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
    /// runtime accept loop, each tagged with the ssh recipe + token to
    /// spawn a child through for that one port (`None` when no host has a
    /// runtime at all).
    /// `proxy_ensured`: the ports this frontend has bound, each with the
    /// `Arm` its listener shares; opening a page on one of them re-arms it,
    /// since the daemon may have refused the port (`bad_port`) since.
    proxy_capable_hosts: std::collections::HashSet<HostKey>,
    proxy_listener_tx: Option<
        tokio::sync::mpsc::UnboundedSender<(
            std::net::TcpListener,
            sot_protocol::ssh_bridge::SshRecipe,
            Option<String>,
            sot_protocol::ssh_bridge::LinkGate,
            std::sync::Arc<crate::proxy_listen::Arm>,
        )>,
    >,
    proxy_ensured: std::collections::HashMap<u16, std::sync::Arc<crate::proxy_listen::Arm>>,
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















































































    fn redraw(&mut self) -> Result<()> {
        if self.help_peek_start_pending {
            self.help_peek_start_pending = false;
            self.help.peek = Some(help::Peek { context: self.help_context(), started: std::time::Instant::now() });
        }
        if self.help_start_pending {
            self.help_start_pending = false;
            self.open_help_drawer(self.help_context());
        }

        self.drain_events();
        // Prune finished status-change flashes; while any is still fading,
        // mark dirty so the frame loop keeps animating it (the fast-repaint
        // cadence is armed in `about_to_wait`).
        if self.prune_expired_flashes(std::time::Instant::now()) {
            self.dirty = true;
        }
        // Coalesced reflow: one MathRendered (or a burst) sets
        // needs_md_reflow; we rebuild preview_md here so the walk pulls
        // the freshly-cached SVG dims when sizing per-block placeholders.
        // Markdown-only by construction — non-markdown previews don't go
        // through the math walk.
        if self.needs_md_reflow {
            self.needs_md_reflow = false;
            if let Some((mime, bytes)) = self.preview_src.clone() {
                if mime == "text/markdown" || mime == "text/x-markdown" {
                    self.render_preview_source(&mime, &bytes);
                }
            }
        }
        // Debounce nav-driven backend round-trips on cursor-settle. User-
        // reported: hold-to-scroll generated hundreds of `preview.get` /
        // `concept.read` / `file.parse` requests per second, saturating
        // the SSH tunnel and pushing wgpu through enough rapid-fire
        // preview blob rasterisation that the AMD driver overlay fired.
        // Cascade: tunnel saturation → transport reconnect → hello-time
        // `tree.root` re-fire → cursor reset to row 0. The fires below
        // are the *only* path that ships per-row backend traffic;
        // suppressing them until the cursor sits still for
        // `NAV_FIRE_DEBOUNCE` makes hold-to-scroll free.
        let cursor_now = (self.mode, self.tree.selected);
        if self.last_cursor_pos != Some(cursor_now) {
            self.last_cursor_pos = Some(cursor_now);
            self.cursor_moved_at = Some(std::time::Instant::now());
        }
        let debouncing = self
            .cursor_moved_at
            .map(|t| t.elapsed() < NAV_FIRE_DEBOUNCE)
            .unwrap_or(false);
        if debouncing {
            // Mark dirty so `about_to_wait` reschedules a redraw at the
            // frame boundary; on each subsequent redraw the elapsed
            // check passes once the user settles, then the fires go
            // through. ~10 cheap no-op redraws per settle, which is
            // dwarfed by the per-row backend traffic we're skipping.
            self.dirty = true;
        } else {
            self.cursor_moved_at = None;
            self.maybe_fire_concept_read();
            self.maybe_fire_preview();
        }
        // Drive `--auto-expand` exactly once, after the initial selection
        // has been applied (i.e., the first TreeRoot/ModulesList landed).
        // We clear the flag whether or not the expansion request actually
        // queued — a no-op row (leaf or already expanded) doesn't deserve
        // a retry loop.
        if self.pending_auto_expand
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            self.try_expand_selected();
            self.pending_auto_expand = false;
        }
        // `--auto-pin`: drive C2 toggle once the cursor selection has
        // landed. Same gating as `--auto-expand`. Pinning a row whose
        // id doesn't start with `files:` is a `toggle_pin` no-op; the
        // flag still clears so we don't churn.
        if self.pending_auto_pin
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            self.toggle_pin();
            self.pending_auto_pin = false;
        }
        // `--demo-repl-eval`: one-shot self-submit once the workspace is
        // live (same gating as the other harness one-shots). Goes through
        // submit_repl_input so a repl_log entry exists for the frames to
        // land in, then shows the REPL drawer so the capture includes it.
        if self.pending_demo_repl_eval.is_some()
            && self.pending_initial_selection.is_none()
            && !self.tree.rows.is_empty()
        {
            if let Some(code) = self.pending_demo_repl_eval.take() {
                self.repl_input = code;
                self.submit_repl_input();
                if self.drawer != DrawerContent::Repl {
                    self.drawer = DrawerContent::Repl;
                }
            }
        }
        // `--demo-function-methods` chain: once the target function row
        // appears in the tree (after the col-2 splice has landed),
        // position cursor on it and fire the methods request. Single-fire
        // by clearing the pending tuple.
        if let Some((module, name)) = self.pending_demo_function_methods.clone() {
            let target_id = format!("modules:{module}:{name}");
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
                self.tree.selected = idx;
                self.try_expand_selected();
                self.pending_demo_function_methods = None;
            }
        }
        // `--start-path` walk (files mode): land the cursor on the target
        // file, expanding one collapsed ancestor directory per tree update
        // on the way down. Once the cursor is on the file row, the normal
        // cursor-tracking passes above (concept.read / preview / file.parse)
        // fire exactly as they would for a user host-2 — which is the
        // point: `--capture-preview` only fires preview.get, but the
        // concept panel and drift badge key off the cursored row.
        if let Some(path) = self.pending_start_path.clone() {
            let target_id = format!("files:{path}");
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
                self.tree.selected = idx;
                self.pending_start_path = None;
                self.start_path_fired = None;
            } else {
                // Deepest ancestor directory that exists in the tree but is
                // still collapsed. (Ancestors appear top-down, so the last
                // match is the frontier of the walk.)
                let mut prefix = String::new();
                let mut frontier: Option<usize> = None;
                for seg in path.split('/') {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(seg);
                    if prefix == path {
                        break; // the file itself is handled above
                    }
                    let anc_id = format!("files:{prefix}");
                    if let Some(idx) = self
                        .tree
                        .rows
                        .iter()
                        .position(|r| r.node.id == anc_id && !r.expanded)
                    {
                        frontier = Some(idx);
                    }
                }
                if let Some(idx) = frontier {
                    let anc_id = self.tree.rows[idx].node.id.clone();
                    // Fire once per frontier; `expanded` flips only when the
                    // children splice lands, so gate re-fires on the memo.
                    if self.start_path_fired.as_deref() != Some(anc_id.as_str()) {
                        self.tree.selected = idx;
                        if self.try_expand_selected() {
                            self.start_path_fired = Some(anc_id);
                        } else {
                            // Not expandable (leaf / no children): the path
                            // can't be reached — stop walking rather than
                            // retry every redraw.
                            tracing::warn!(%path, %anc_id, "--start-path dead end — ancestor not expandable");
                            self.pending_start_path = None;
                            self.start_path_fired = None;
                        }
                    }
                }
                // No ancestor row yet (root still loading): stay pending;
                // the next tree update re-enters this block.
            }
        }

        // Single preview pane rect that the preview-layer surface draws
        // into. The exact source (PNG quad / SVG quad / cosmic-text
        // markdown buffer / cosmic-text concept buffer) is picked below
        // via priority cascade so the pane "switches based on context"
        // per the user's layout intent.
        let mut preview_cells = ratatui::layout::Rect::default();
        // Cell-rect of the bottom drawer (REPL/Terminal/Monitor share it),
        // carried out of the closure the same way as `preview_cells` so the
        // Ctrl+M monitor chart quad can be sized to the drawer rect after the
        // draw returns.
        let mut repl_cells = ratatui::layout::Rect::default();
        // Scrollback sub-rect + visible line window, exported for the
        // inline REPL image paint pass (same borrow pattern as repl_cells).
        let mut repl_scrollback_cells = ratatui::layout::Rect::default();
        let mut repl_window: (usize, usize) = (0, 0);
        // Cache the four pane content rects + clamped REPL scroll across
        // the closure. Same pattern as `preview_cells` — captured by
        // mutable borrow inside the draw closure, then written back to
        // self after `draw` returns.
        let mut new_pane_rects = self.pane_rects;
        let mut new_repl_scroll = self.repl_scroll;
        // Nav-spill overlay rows collected by this draw (same captured-
        // local pattern as `preview_cells`); written to
        // `self.nav_spill_segments` after `draw` returns for the render-
        // pass tail to paint.
        let mut nav_spill_segs_out: Vec<NavSpillSeg> = Vec::new();

        // A NavTree prompt, the not-ended count or `closing…`, and the lease
        // notice are pinned under the nav list (`nav_pinned_rows`), so no
        // scroll hides what Enter would confirm. A text prompt is the
        // input field the user types into; a block cursor (▏) marks the
        // insertion point.
        let status = self.status.clone();
        let nav_prompt_line = match &self.nav_prompt {
            Some(NavPrompt::CreateFile { input, .. }) => Some((format!("new file or dir/: {input}▏"), String::new())),
            Some(NavPrompt::ConfirmDelete { label, .. }) => Some((format!("delete {label}? [y/N]"), String::new())),
            Some(NavPrompt::ScaleEntry { input, .. }) => Some((format!("pixel size (nm): {input}▏"), String::new())),
            Some(NavPrompt::ConfirmQuit { keep }) => Some(quit_prompt_line(*keep)),
            None => None,
        };
        // The counts owed, read once: this frame draws their sum, and the frame that
        // presents it whole acks exactly these; then the line holds as `not_ended_shown`.
        let owed = self.leases.owed();
        let owed_line = crate::lease::not_ended_line(owed.iter().map(|(_, n)| n).sum());
        let nav_line = self.leaving.as_ref().and_then(|l| l.line()).or_else(|| owed_line.clone()).or_else(|| {
            let now = std::time::Instant::now();
            self.not_ended_shown.as_ref().filter(|(_, until)| now < *until).map(|(l, _)| l.clone())
        });
        let mut owed_drawn = false;
        let mut leaving_drawn = false;
        let lease_notice = self.leases.notice();
        // Local wall-clock of the machine running the frontend, sampled once
        // per frame and turned into the top-right chrome clock text at the
        // paint site below (`clock_label`, which also prefixes the date
        // when there's room). `chrono::Local` is cross-platform (same
        // behaviour on Windows/macOS/Linux); the once-per-second repaint is
        // scheduled in `about_to_wait`, which also covers the date rolling
        // over at midnight — no separate timer needed.
        let clock_now = chrono::Local::now().naive_local();
        // Battery readout painted just left of the clock. The OS query isn't
        // free, so refresh the cache at most once per `BATTERY_QUERY_INTERVAL`
        // (the clock repaints ~1×/s and reuses the cached value between
        // refreshes). `None` => no battery / query failed => paint nothing.
        self.refresh_battery_label();
        let battery = self.battery_label.clone();
        let last_key = self.last_key.clone();
        // FE/BE version stamp for the bottom chrome edge. Snapshotted here
        // with the other draw locals because the draw closure can't borrow
        // `self` again.
        let (version_stamp, version_skew) = version_label(
            &sot_protocol::app_version(),
            self.backend_version.as_deref(),
        );
        let mode = self.mode;
        let focus = self.focus;
        let help_context = self.help_context();
        let maximize_slot =
            maximize_slot(self.maximized, focus, self.leaving.as_ref().and_then(|l| l.line()).is_some());
        // State-nav selected-session contrast lever, snapshotted for the draw
        // closure (it mustn't borrow `self`).
        let contrast_dim = self.contrast_dim;
        // Owner ruling (2026-09-06): the wordmark gets the nav pane's own first
        // row, top-left -- right-aligned on the first tree row it collided with
        // text on every non-ultrawide. Snapshotted here: the draw closure must
        // not borrow `self`.
        let nav_logo_row = self.wordmark_quad.is_some();
        // Inline REPL figures, pass 1: decode any Image frame that has no
        // quad yet (base64 → RGBA → texture) and prune entries that aged
        // out of the log. Runs here, outside the draw closure, so texture
        // upload never contends with the frame's borrows.
        {
            let mut new_quads: Vec<((u64, usize), ReplImage)> = Vec::new();
            for entry in &self.repl_log {
                for (fi, fr) in entry.frames.iter().enumerate() {
                    if let sot_protocol::ReplFrame::Image { data_base64, .. } = fr {
                        let key = (entry.eval_id, fi);
                        if self.repl_images.contains_key(&key) {
                            continue;
                        }
                        use base64::Engine as _;
                        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(data_base64)
                        else {
                            continue;
                        };
                        let Ok(img) = image::load_from_memory(&raw) else {
                            continue;
                        };
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        if let Ok(quad) = Quad::from_rgba8(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &rgba,
                            w,
                            h,
                        ) {
                            new_quads.push((key, ReplImage { quad, w, h }));
                        }
                    }
                }
            }
            for (k, v) in new_quads {
                self.repl_images.insert(k, v);
            }
            if !self.repl_images.is_empty() {
                let log = &self.repl_log;
                self.repl_images
                    .retain(|k, _| log.iter().any(|e| e.eval_id == k.0));
            }
        }
        // Pass 2: build the drawer lines, reserving rows for decoded
        // figures. Fit width comes from LAST frame's scrollback sub-rect —
        // the natural answer to the build-before-layout chicken-egg (review
        // note: NOT monitor_rect_px, which is the Ctrl+M drawer's rect).
        // One frame of lag on a resize, self-corrects; 0 before the
        // drawer's first draw, where the caption fallback covers the gap.
        let (repl_lines, repl_slots, repl_starts) = build_repl_lines(
            &self.repl_log,
            &self.repl_images,
            self.repl_scrollback_px.w,
            self.repl_scrollback_px.h,
            self.cell_w,
            self.cell_h,
            self.active_repl_starting(),
        );
        self.repl_image_slots = repl_slots;
        let build_key = (
            self.repl_scrollback_px.w.to_bits(),
            self.repl_scrollback_px.h.to_bits(),
            self.cell_w.to_bits(),
            self.cell_h.to_bits(),
        );
        if let Some((prev_key, anchor_id, anchor_span)) = self.repl_build_anchor {
            if prev_key == build_key {
                new_repl_scroll = pinned_repl_scroll(
                    new_repl_scroll,
                    anchor_id,
                    anchor_span,
                    repl_lines.len(),
                    &repl_starts,
                );
            }
        }
        self.repl_build_anchor = repl_starts
            .last()
            .map(|&(id, start)| (build_key, id, repl_lines.len().saturating_sub(start)));
        let repl_input = self.repl_input.clone();
        let repl_pkg_mode = self.repl_pkg_mode;
        // The navigation body begins with status + spacer. The picker adds
        // two path/header rows before its entries. Keep scroll and hit testing
        // aligned when the help legend moves from the body to the border.
        let (nav_cursor_body_pos, nav_has_cursor) = match &self.workspace_picker {
            Some(p) => (4usize.saturating_add(p.selected), !p.entries.is_empty()),
            None => (
                2usize.saturating_add(self.tree.selected),
                !self.tree.rows.is_empty(),
            ),
        };
        // Mutable copy of the persistent scroll. The draw closure updates
        // this in place based on the cursor's viewport position; the
        // result is written back to self.tree_scroll after the draw.
        let mut nav_scroll = self.tree_scroll;
        self.fire_due_read_mark();
        if self.pane_attach_term.is_some() {
            self.pump_pane_attach_term();
        }
        // Local terminal drawer (G2/G3): lazily spawn the OS shell the
        // first time the Terminal drawer is shown, then drain any pending
        // output into its parser before we borrow its screen for the draw.
        // All mutation happens here, before the `self.terminal.draw`
        // borrow and the immutable `local_term` screen borrow below.
        // ADR 0041 step 6 U3: `drawer.attach_only` picks the Terminal
        // drawer's BACKEND, once, the first time the drawer opens this
        // session. Off (the default) or off-Windows: `use_attach_only`
        // is always `false` and every line below behaves exactly as it
        // did before this unit — "When off, NOTHING the FE does today
        // changes."
        #[cfg(windows)]
        let use_attach_only = drawer_uses_attach(
            self.settings.attach_only,
            self.attach_term.is_some(),
            self.local_term.is_some(),
            &self.leases.granted_state_roots(),
            self.own_state_root.as_deref(),
        );
        #[cfg(not(windows))]
        let use_attach_only = false;

        // ADR 0041 step 6 U3 ruling (a), Codex review round finding 2:
        // spawn (gated on the drawer being open) is separate from pump
        // (which runs on EVERY redraw regardless of drawer visibility).
        #[cfg(windows)]
        if self.drawer == DrawerContent::Terminal && use_attach_only && self.attach_term.is_none() {
            self.spawn_attach_term();
        }
        #[cfg(windows)]
        if self.attach_term.is_some() {
            self.pump_attach_term();
        }

        if self.drawer == DrawerContent::Terminal && !use_attach_only {
            if self.local_term.is_none() {
                #[cfg(windows)]
                if self.settings.attach_only {
                    self.status =
                        "attach-only terminal needs this computer's backend to hold this window; opened a plain terminal"
                            .to_string();
                }
                let shell = crate::term::resolve_shell(self.settings.terminal_shell.as_deref());
                let waker = self.window.clone();
                // cwd = repo root, so the plain shell starts in the
                // project directory. ADR 0017.
                let cwd = self.repo_dir.clone();
                match crate::term::LocalTerminal::spawn(
                    &shell,
                    80,
                    24,
                    cwd.as_deref(),
                    Box::new(move || waker.request_redraw()),
                ) {
                    Ok(t) => {
                        tracing::info!(program = %shell.program, "local terminal spawned");
                        self.local_term = Some(t);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to spawn local terminal");
                        self.status = format!("terminal spawn failed: {e}");
                        // Fall back to closing the drawer so the user isn't
                        // staring at an empty pane with no explanation.
                        self.drawer = DrawerContent::Closed;
                    }
                }
            }
            if let Some(t) = self.local_term.as_mut() {
                let processed = t.pump();
                // Diagnostic surfaced on the status line so a blank pane is
                // debuggable without RUST_LOG: parser size, dead flag, and
                // whether the screen currently holds any non-blank cell.
                // Only overwrite the status line when the pane actually looks
                // wrong (dead / no content) — otherwise it fires every frame
                // the drawer is open and clobbers real status messages
                // (connection line, pin/unpin, ADR-0019 `notify`).
                let dead = t.is_dead();
                let screen = t.screen();
                let (srows, scols) = screen.size();
                let mut has_content = false;
                'scan: for r in 0..srows {
                    for c in 0..scols {
                        if let Some(cell) = screen.cell(r, c) {
                            if !cell.contents().is_empty() {
                                has_content = true;
                                break 'scan;
                            }
                        }
                    }
                }
                if dead || !has_content {
                    self.status = format!(
                        "term: {scols}x{srows} content={has_content} dead={dead} pumped={processed}"
                    );
                }
            }
        }
        // Re-read after a possible spawn-failure close above; this is the
        // value the renderer branches on.
        let drawer = self.drawer;
        // Borrow the LLM terminal screen for the duration of the draw.
        // `pane_attach_term`'s `vt100-ctt` `screen()` returns a `&Screen`
        // tied to the client; since `terminal.draw` borrows a different
        // field (self.terminal), Rust's split-borrow rules let us hold
        // both at once. The local terminal's screen is borrowed the same
        // way when the Terminal drawer is active. Unconditional on every
        // platform since ADR 0045 decision 1 (a capsule row attaches
        // through its own daemon everywhere).
        //
        // LU6a: which source actually wins is `pane_screen_choice`'s
        // call, not an unconditional `pane_attach_term`-if-present — a
        // live but not-yet-checkpointed client (or a still-`Pending`
        // feed) defers to `pane_hold` so a capsule switch never paints
        // the new client's empty parser. Coordinator amendment: a client
        // that went terminal before ever checkpointing falls all the way
        // through to blank instead (`pane_screen_choice`'s own doc) —
        // three separate `let`s (rather than inlining each as a call
        // argument) so the one `&mut` read (`is_dead`) never overlaps
        // the `&ref` reads around it.
        let pane_attach_has_client = self.pane_attach_term.is_some();
        let pane_attach_checkpointed =
            self.pane_attach_term.as_ref().is_some_and(|t| t.is_checkpointed());
        let pane_attach_is_dead = self.pane_attach_term.as_mut().is_some_and(|t| t.is_dead());
        let pane_screen = pane_screen_choice(
            pane_attach_has_client,
            pane_attach_checkpointed,
            pane_attach_is_dead,
            self.pane_feed,
            self.pane_hold.is_some(),
        );
        // ADR 0030 §8 "Where it is shown", widened by ADR 0045 decision 1
        // (Codex review): paints whenever the client is alive but not
        // honestly attached — dead-uncheckpointed (the original case),
        // a mid-outage `Unreachable` retry, a refusal, or a failure AFTER
        // checkpointing (the frozen screen underneath is real, but
        // stale). Codex review: reads the RETAINED client's own
        // `status_line()` directly, never `self.status` — that field is
        // shared with every other status-bar message in the whole event
        // loop and a later, unrelated write (autostart, a daemon
        // reconnect, a drawer switch) would silently retitle this pane's
        // own explanation to whatever last touched the status bar.
        let pane_attach_status = self.pane_attach_term.as_ref().map(|t| t.status_line());
        let pane_attach_is_attached = pane_attach_status == Some("attached");
        // A daemon that refused this frontend's protocol is the root cause of
        // whatever the client or the dial reports, so its line leads.
        let pane_host = self
            .bl_pane_target
            .as_ref()
            .map(|(h, _)| h)
            .unwrap_or(&self.active_host);
        let pane_terminal_reason: Option<String> = pane_reason_line(
            self.protocol_mismatch
                .get(pane_host)
                .and_then(|m| m.lines().next()),
            pane_terminal_reason_text(
                pane_shows_terminal_reason(pane_attach_has_client, pane_attach_is_attached),
                pane_attach_status,
            ),
        // SHOULD-FIX (Codex review, lane B5 discharge): no live client at
        // all (a dial that never got to attach in the first place) still
        // needs a persistent, non-clobberable reason when this row's
        // host has a known-broken dial — same priority tier as a live
        // client's own failure.
            self.pane_dial_error.as_deref(),
        );
        let pane_overlay = pane_overlay_lines(
            pane_terminal_reason,
            pane_discard_notice(
                self.pane_inputs_discarded,
                self.pane_attach_term.as_ref().map_or(0, |t| t.inputs_discarded()),
            ),
        );
        // Switch-latency Phase 1, item 3: the acceptance metric itself
        // (keypress → current screen visible), not merely the client's
        // own parser being ready (`pump_pane_attach_term`'s "checkpoint
        // applied") — this is the first REDRAW that actually paints the
        // new client's own screen (`PaneScreen::Client`) rather than the
        // held prior content or the tmux fallback. One-shot per attach,
        // same edge-triggered pattern as the other attach-outcome lines.
        if pane_screen == PaneScreen::Client && !self.pane_attach_presented {
            self.pane_attach_presented = true;
            let since_request_ms = self
                .pane_attach_requested_at
                .map(|s| s.elapsed().as_millis() as u64)
                .unwrap_or(0);
            tracing::info!(since_request_ms, "session pane: capsule screen presented");
        }
        let blank_pty_screen;
        let pty_screen = match match pane_screen {
            PaneScreen::Client => self.pane_attach_term.as_ref().map(|t| t.screen()),
            PaneScreen::Hold => self.pane_hold.as_ref().map(|h| h.screen()),
            PaneScreen::Empty => None,
        } {
            Some(s) => s,
            None => {
                let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                blank_pty_screen = blank_pane_screen(cols, rows);
                &blank_pty_screen
            }
        };
        #[cfg(windows)]
        let attach_screen = self.attach_term.as_ref().map(|t| t.screen());
        #[cfg(not(windows))]
        let attach_screen: Option<&vt100::Screen> = None;
        let term_screen = if drawer == DrawerContent::Terminal {
            self.local_term
                .as_ref()
                .map(|t| t.screen())
                .or(attach_screen)
        } else {
            None
        };
        let llm_selection = self.llm_selection;
        // Captured by the closure and written when the LLM pane's
        // content rect is final; read after the closure to decide
        // whether to fire `pty.open` / `pty.resize`.
        let mut pty_size_observed: (u16, u16) = (0, 0);
        // Same idea for the local terminal drawer: capture the final
        // drawer rect in the closure, resize the PTY to match after.
        let mut term_size_observed: (u16, u16) = (0, 0);
        // Annotation snapshot for the chrome status line. `fired` is what
        // we asked the backend about; `cached` matches when the response
        // is in hand for the current cursor. Three states: no target
        // (None/None), loading (Some/None or mismatched), and ready
        // (Some/Some with the same target).
        let concept_target = self.concept_target_fired.clone();
        let concept_status: String = match (&concept_target, &self.concept) {
            (None, _) => "annotation: (no target for this row)".to_string(),
            (Some(t), Some(info)) if info.target == *t => {
                if info.exists {
                    let drift = match (
                        info.synced_against.as_deref(),
                        info.target.strip_prefix("files/"),
                    ) {
                        (Some(synced), Some(path)) => match self.file_ast_hashes.get(path) {
                            Some(h) if h == synced => " · in sync",
                            Some(_) => " · STALE (file ast_hash differs from synced_against)",
                            None => match self.file_parse_retry.get(path) {
                                Some(&(_, n)) if n >= FILE_PARSE_MAX_RETRIES => {
                                    " · drift check unavailable (file.parse failing)"
                                }
                                _ => " · checking…",
                            },
                        },
                        (Some(_), None) => " · sync check N/A",
                        (None, _) => " · no synced_against frontmatter",
                    };
                    format!("annotation: present — {t}{drift}")
                } else {
                    format!("annotation: (none) — {t}")
                }
            }
            (Some(t), _) => format!("annotation: loading — {t}"),
        };
        // Drift detection for the cursored row: we have both pieces of
        // information cached only for the selection (concept.read fires on
        // cursor move; file.parse fires once per visited path). Stale when
        // the annotation parses a `synced_against` AND the file's
        // `ast_hash` differs. Expanding to non-cursored rows needs a
        // per-row concept cache — phase 2.
        let selected_stale: bool = match (self.tree.rows.get(self.tree.selected), &self.concept) {
            (Some(row), Some(info)) if info.exists => {
                if let (Some(synced), Some(path)) = (
                    info.synced_against.as_ref(),
                    row.node.id.strip_prefix("files:"),
                ) {
                    self.file_ast_hashes
                        .get(path)
                        .map(|h| h != synced)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        };
        // Snapshot per-row chrome strings up-front so the ratatui closure
        // doesn't need to borrow `self.tree` (the closure captures `frame`
        // mutably elsewhere and the borrow checker dislikes mixing).
        //
        // When the workspace picker is active we render *its* directory
        // listing in the NavTree pane instead of `self.tree.rows`, so
        // Sessions mode flow visibly transitions to the picker without
        // having to introduce a second pane region. A title row at the
        // top shows `current_path` so the user always knows where they
        // are; below it, each subdirectory is one row, plus a `[..]`
        // ascend row at the very top of the list for one-key parent
        // navigation.
        // Each tuple: (line text, selected, stale-annotation, pinned, agent
        // tone, flash). The 5th element is the state-nav agent work-state (ADR
        // 0023), present only on Sessions rows that carry one — `None`
        // everywhere else, so every other mode renders unchanged. The 6th is
        // the status-change flash factor (0.0 = no flash), also Sessions-only.
        // (text, is_selected, is_stale, is_pinned, agent_tone, flash,
        //  is_pending, is_attention)
        // `is_attention` (2026-09-15, field report) marks a row that
        // names an action the user must take before the default is
        // committed: yellow + bold, ahead of every other colour layer.
        // `is_pending` (ADR 0025 §1 badge floor) flags a Sessions row whose
        // workspace has a pending nav.preview result waiting — rendered as a
        // non-disruptive indicator distinct from the work-state colours.
        type NavRow = (
            String,
            bool,
            bool,
            bool,
            Option<(AgentTone, bool)>,
            f32,
            bool,
            bool,
        );
        let (tree_lines, tree_empty): (Vec<NavRow>, bool) = if let Some(p) = &self.workspace_picker
        {
            let mut rows: Vec<NavRow> = Vec::with_capacity(p.entries.len() + 4);
            rows.push((
                format!("workspace picker · {}", p.current_path),
                false,
                false,
                false,
                None,
                0.0,
                false,
                false,
            ));
            // Two footer rows: NAVIGATION first (→ is how you descend into
            // a folder — Enter does NOT, it creates), then the create keys.
            // Splitting them stops the common muscle-memory error of hitting
            // Enter to open a folder and instead spawning a session.
            rows.push((
                format!("  {} into · {} up · {} move · {} cancel",
                    self.bindings.first_label(Action::NavExpand), self.bindings.first_label(Action::NavCollapse),
                    self.bindings.first_label(Action::NavDown), self.bindings.first_label(Action::Cancel)),
                false,
                false,
                false,
                None,
                0.0,
                false,
                false,
            ));
            rows.push((
                format!("  {} Claude · {} bare · {} Codex", self.bindings.first_label(Action::SessionCreate),
                    self.bindings.first_label(Action::SessionCreateBare), self.bindings.first_label(Action::SessionCreateCodex)),
                false,
                false,
                false,
                None,
                0.0,
                false,
                false,
            ));
            // Per-session accounts (owner-simplified brief, 2026-09-15):
            // hidden entirely when the daemon reports only "default" (or
            // never answered `accounts.list` — same empty state). A
            // never-logged-in folder is a NORMAL choice, not an error —
            // the row's own pane runs the login on first start — so it's
            // still selectable, just marked; this row renders with the
            // same dim treatment as the two footer rows above (no
            // agent/pinned/stale/selected tone applies to it).
            if p.account_choice_visible() {
                let acct = &p.accounts[p.account_selected];
                let marker = if acct.any_logged_in() { "" } else { " (not logged in)" };
                rows.push((
                    format!("  account: {}{marker} · {} next", acct.name,
                        self.bindings.first_label(Action::SessionAccountNext)),
                    false,
                    false,
                    false,
                    None,
                    0.0,
                    false,
                    // The only picker row naming a key you must press
                    // BEFORE Enter: Enter commits the default account
                    // immediately, so a dim hint here is one a user reads
                    // past — reported from the field, 2026-09-15.
                    true,
                ));
            }
            for (i, e) in p.entries.iter().enumerate() {
                let selected = i == p.selected;
                let caret = if selected { ">" } else { " " };
                let disclosure = if e.has_children { "▸" } else { "·" };
                rows.push((
                    format!("{caret} {disclosure} {}/", e.name),
                    selected,
                    false,
                    false,
                    None,
                    0.0,
                    false,
                    false,
                ));
            }
            let empty = p.entries.is_empty();
            (rows, empty)
        } else {
            let pinned_id = self.pinned_preview_node_id.as_deref();
            let now = chrono::Utc::now();
            let flash_now = std::time::Instant::now();
            let rows: Vec<NavRow> = self
                .tree
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let selected = i == self.tree.selected;
                    // ADR 0030 §8 decision 31c: a capsule row held by a
                    // foreign build folds into the SAME "stale" colour
                    // slot as annotation drift (both are cross-cutting
                    // yellow, never mode-specific) — these two conditions
                    // never both hold in practice (concept-annotation
                    // staleness is computed only for `files:`-prefixed
                    // rows, `is_foreign` only for `kind == "session"`
                    // ones), so sharing the flag adds no new ambiguity.
                    let is_foreign = capsule_row_is_foreign(
                        &r.node.kind,
                        r.node.payload.get("phase").and_then(|v| v.as_str()),
                    );
                    let stale = (selected && selected_stale) || is_foreign;
                    let pinned = pinned_id == Some(r.node.id.as_str());
                    // Agent tone + status-change flash only on Sessions
                    // rows (kind "session"), keyed by the row's slug so it
                    // matches the bottom strip. `pending` (badge floor, ADR
                    // 0025 §1) is set when that workspace has a pending
                    // nav.preview result waiting, keyed by the row's own
                    // (host, slug) — ADR 0042 L2a codex review item E: a
                    // slug-only check couldn't tell this host's row apart
                    // from another host's same-slug pending badge.
                    let (agent, flash, pending) = if r.node.kind == "session" {
                        let slug = r.node.payload.get("slug").and_then(|v| v.as_str());
                        let host = r.node.payload.get("host").and_then(|v| v.as_str());
                        let flash = host
                            .zip(slug)
                            .map(|(h, s)| self.flash_factor_for(h, s, flash_now))
                            .unwrap_or(0.0);
                        let pending = host
                            .zip(slug)
                            .map(|(h, s)| {
                                self.pending_nav
                                    .contains_key(&(h.to_string(), s.to_string()))
                            })
                            .unwrap_or(false);
                        (agent_tone_for(&r.node.payload, now), flash, pending)
                    } else {
                        (None, 0.0, false)
                    };
                    (
                        format_tree_row(r, selected, pinned),
                        selected,
                        stale,
                        pinned,
                        agent,
                        flash,
                        pending,
                        // No nav tree row is an attention row: the slot
                        // exists for picker affordances, not for content.
                        false,
                    )
                })
                .collect();
            let empty = self.tree.rows.is_empty();
            (rows, empty)
        };
        // Transient nav spill: a nav-cursor move (re)arms the timer; while
        // it runs, nav rows whose text overflows the nav column render
        // their FULL text as a floating overlay across the preview pane's
        // left edge (segment collection below in the draw closure; painted
        // by the overlay text layer at the end of the render pass — pane
        // geometry never moves). It vanishes `[nav] spill_ms` after the
        // last move. Detected here frame-side, by diffing the cursor
        // tuple, instead of in every input path — keys, wheel, mode and
        // workspace switches all trigger uniformly. The picker cursor
        // rides in the tuple so workspace-picker browsing spills too.
        let spill_cursor_now = (
            self.mode,
            self.active_workspace_id.clone(),
            self.tree.selected,
            self.workspace_picker.as_ref().map(|p| p.selected),
            self.tree.generation,
        );
        if self.nav_spill_cursor.as_ref() != Some(&spill_cursor_now) {
            // Arm ONLY on a user cursor move: same mode+workspace, same tree
            // CONTENT (generation), different cursor. Everything else that
            // perturbs the tuple — boot, the async initial tree load, a
            // cursor restore, a refresh splicing rows around the cursor, a
            // mode/workspace switch swapping the tree — updates the baseline
            // without arming (codex review: first-frame-only suppression
            // made startup spill timing-dependent).
            let user_move = match (self.nav_spill_cursor.as_ref(), &spill_cursor_now) {
                (Some((pm, pw, ps, pp, pg)), (m, w, s, p, g)) => {
                    pm == m && pw == w && pg == g && (ps != s || pp != p)
                }
                (None, _) => false,
            };
            self.nav_spill_cursor = Some(spill_cursor_now);
            if user_move && self.settings.nav_spill_ms > 0 {
                self.nav_spill_until = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_millis(self.settings.nav_spill_ms),
                );
            }
        }
        let nav_spill_active = self
            .nav_spill_until
            .map(|u| std::time::Instant::now() < u)
            .unwrap_or(false);
        // Layout proportions from the user's settings file (or
        // defaults). Snapshotted here so the ratatui closure doesn't
        // borrow `self`. Maximisation overrides the geom inside the
        // closure by passing a `maximize_slot`. Wide-preview rewrites
        // the preset itself (Llm column dropped, width to Preview) so
        // layout::compute needs no new inputs — the Llm-less path is
        // the same one the portrait preset already exercises.
        let mut layout_preset = {
            let p = self.settings.resolve_preset(self.monitor_aspect);
            if self.wide_preview {
                p.wide_preview()
            } else {
                p.clone()
            }
        };
        if drawer == DrawerContent::Help && layout_preset.drawer.is_none() {
            layout_preset.drawer = Some(crate::settings::Slot::Repl);
        }
        // `drawer` was bound above (after the terminal lazy-spawn/close).
        let drawer_open = drawer.is_open();
        // T1: full path of the file the preview is showing, snapshotted here
        // so the draw closure doesn't borrow `self`.
        let preview_name = self.preview_pane_name();
        // 4.3: the monitor tab label's source, snapshotted here (String +
        // bool) for the same reason as `preview_name` above — the draw
        // closure must not borrow `self`.
        let monitor_host_label = self.monitor_host();
        let monitor_host_connected = self.conns.iter().any(|(h, _)| h == &monitor_host_label);
        // Sessions create-legend gate: is the workspace picker open? Snapshotted
        // here (Copy bool) so the header inside the draw closure can decide
        // whether to show the standalone three-key create legend without
        // borrowing `self`. Suppressed while the picker is open — the picker's
        // own footer already carries the legend inline.

        // Borrowed LAST, after every `&mut self` pump above (the Windows-only
        // attach-term pumps included): the draw closure captures these two
        // references, so taking them any earlier spans those mutations and
        // fails the borrow check on the platform that has them.
        let help_state = &self.help;
        let help_bindings = &self.bindings;
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                // Inner divisions positioned by the user-configurable
                // settings (defaults 50/50, see settings.toml). Range
                // clamped to [10, 90] at parse time so the math here
                // can't degenerate.
                // Pane geometry — pure integer math, no Block borders.
                // Borders are drawn by us into the buffer below so every
                // shared edge is exactly one cell wide and junctions are
                // proper line-drawing characters. The "content" rects
                // are the interior of each quadrant (no border cells).
                //
                //   col 0 = outer left   col mid_col = inner vertical   col last = outer right
                //   row 0 = outer top    row mid_row = inner horizontal row last = outer bottom
                // Preset-driven geometry (ADR 0014 layout rework).
                // Each named slot gets a rect; vlines/hlines drive the
                // wireframe + title positioning. Maximisation collapses
                // every other slot + every inner border so the focused
                // pane absorbs the area; zero-sized siblings' paint
                // paths no-op (the pty.open/resize guard at
                // `cols >= 2 && rows >= 2` similarly keeps the BL
                // backend safe). Toggle: Ctrl+z. A leave's line restores the
                // panes (`maximize_slot`).
                let geom = crate::layout::compute(area, &layout_preset, drawer_open, maximize_slot);
                // Names preserved so the rest of the closure reads
                // unchanged: nav = old TL (left column), preview = old
                // TR (middle column), llm = old BL (rightmost column
                // in the 3-col layout), repl = old BR (bottom drawer).
                // `nav_frame_rect` is the pane as laid out (title and focus border
                // hang off it); `nav_rect` is what the tree body may use -- one
                // row shorter when the wordmark owns the first row.
                let nav_frame_rect = geom.rect_for(crate::settings::Slot::Nav);
                let nav_rect = if nav_logo_row && nav_frame_rect.height > 1 {
                    ratatui::layout::Rect {
                        y: nav_frame_rect.y + 1,
                        height: nav_frame_rect.height - 1,
                        ..nav_frame_rect
                    }
                } else {
                    nav_frame_rect
                };
                let preview_rect = geom.rect_for(crate::settings::Slot::Preview);
                let llm_rect = geom.rect_for(crate::settings::Slot::Llm);
                let repl_rect = geom.rect_for(crate::settings::Slot::Repl);

                // Style palette: borders are uniform gray; focus is
                // signalled only through title colour (cyan when the
                // pane has focus, gray otherwise). No per-pane border
                // colour means the wireframe stays internally
                // consistent.
                let border_style = Style::default().fg(Color::DarkGray);
                let focus_title_style = Style::default().fg(Color::LightCyan);
                let idle_title_style = Style::default().fg(Color::DarkGray);

                let nav_focus = focus == PaneFocus::NavTree;
                let nav_title = format!(
                    " nav · mode: {} {} ",
                    mode.label(),
                    if nav_focus { "· [FOCUS]" } else { "" }
                );
                // Help lives on the focused border; keep the nav header compact.
                // The status text wraps at the pane's width (a toast is a
                // sentence): "status: " heads the first line only. Everything
                // below counts from body_lines.len(), and the cursor's body
                // position (a header of one status line and a spacer, computed
                // before the draw) shifts by the extra lines here.
                let nav_w = nav_rect.width as usize;
                let (nav_list_h, nav_pinned, line_whole) = nav_pinned_rows(
                    nav_prompt_line.as_ref().map(|(t, c)| (t.as_str(), c.as_str())),
                    lease_notice,
                    nav_line.as_deref(),
                    nav_w,
                    nav_rect.height as usize,
                );
                let nav_list_rect = ratatui::layout::Rect { height: nav_list_h as u16, ..nav_rect };
                // Only a line drawn whole is acked: a grant's count here, the
                // leaving line once presented (`Leaving::presented`).
                owed_drawn = line_whole && owed_line.is_some() && nav_line == owed_line;
                leaving_drawn = line_whole;
                let mut body_lines = status_spans(&status, nav_w);
                let nav_cursor_body_pos = nav_cursor_body_pos + body_lines.len() - 1;
                body_lines.push(RtLine::from(""));
                if tree_empty {
                    body_lines.push(RtLine::from(vec![Span::styled(
                        "  (no tree yet)",
                        Style::default().add_modifier(Modifier::DIM),
                    )]));
                }
                // Exact tree-row span of body_lines, captured AT ASSEMBLY
                // (codex round 4: the header is not a constant — Files/
                // Modules carry 4 chrome lines, Sessions 5, the picker 3 —
                // so any fixed offset either spills chrome or misses bottom
                // rows). The picker's own header row lives inside
                // tree_lines and stays spill-eligible on purpose: floating
                // the full picker path is exactly what the spill is for.
                let tree_rows_body_start = body_lines.len();
                for (
                    text,
                    is_selected,
                    is_stale,
                    is_pinned,
                    agent,
                    flash,
                    is_pending,
                    is_attention,
                ) in
                    &tree_lines
                {
                    let mut style = Style::default();
                    // Cross-cutting colour layer. `is_stale` (annotation
                    // drift OR, since ADR 0030 §8 decision 31c, a foreign-
                    // build capsule row) is loudest and checked FIRST — a
                    // row that is drifted or unusable must read that way
                    // regardless of any work-state tone it also carries.
                    // Below that, state-nav agent tone (ADR 0023) owns the
                    // colour of a Sessions row that has one: working/idle/
                    // blocked/done each get a hue, a stale "working" wilts
                    // (DIM), and selection still reads through the `>`
                    // caret + bold so the cursor stays visible over the
                    // state colour. Without an agent tone either, the
                    // original layer applies: the pinned accent (bright
                    // cyan, distinct from the yellow stale/selected hues),
                    // then selection (light yellow), then dim.
                    if *is_attention {
                        // Attention row: yellow + BOLD. Scoped to rows that
                        // announce a key the user must press BEFORE the
                        // default commits, so it never competes with the
                        // cross-cutting stale hue below (which stays plain
                        // yellow — drift is noticed, not shouted).
                        style = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
                    } else if *is_stale {
                        style = style.fg(Color::Yellow);
                    } else if let Some((tone, aged)) = agent {
                        // Resolve the tone to RGB through the shared contrast
                        // helper so the nav row and the bottom strip render
                        // the same pixels. `Color::Rgb` (not the named tone
                        // colour) is required because the "bright"/"dim"
                        // levers and the status-change flash scale/lerp the
                        // channels — ratatui can't lerp a named colour. Bold
                        // still composes with the colour, and the
                        // stale-"working" wilt still DIMs.
                        let (rgb, bold, dim) =
                            contrast_tone_rgb(*tone, *aged, *is_selected, contrast_dim, *flash);
                        if let Some((r, g, b)) = rgb {
                            style = style.fg(Color::Rgb(r, g, b));
                        }
                        if dim {
                            style = style.add_modifier(Modifier::DIM);
                        }
                        if bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                    } else if *is_pinned {
                        style = style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
                    } else if *flash > 0.0 {
                        // Tone-less Sessions row that just changed state:
                        // resolve the base fg + flash toward white so the
                        // blink reads even without a state colour.
                        let base = if *is_selected {
                            (245, 245, 67)
                        } else {
                            (204, 204, 204)
                        };
                        let (r, g, b) = lerp_to_white(base, *flash);
                        style = style.fg(Color::Rgb(r, g, b));
                        if *is_selected {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                    } else if *is_selected {
                        style = style.fg(Color::LightYellow);
                    } else if contrast_dim && mode == Mode::Sessions {
                        // "dim" lever: fade non-selected Sessions rows that
                        // carry no tone so the selection pops by contrast.
                        // Scoped to Sessions so Files/Modules nav is untouched.
                        let (r, g, b) = scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR);
                        style = style.fg(Color::Rgb(r, g, b));
                    } else {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    // Badge floor (ADR 0025 §1): a workspace with a pending
                    // nav.preview result gets a non-disruptive "result waiting"
                    // badge — a leading `● ` sigil in bright white + bold,
                    // and the row fg pulled to the same accent (clearing DIM) so
                    // it reads distinctly from the work-state tones (green
                    // working / purple waiting / red blocked / etc.) and the
                    // cyan pin, without adding another hue to the palette. The
                    // view is never switched; only the colour/sigil changes.
                    if *is_pending {
                        const PENDING_ACCENT: Color = Color::Rgb(255, 255, 255);
                        style = style.fg(PENDING_ACCENT).remove_modifier(Modifier::DIM);
                        if *is_selected {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        body_lines.push(RtLine::from(vec![
                            Span::styled(
                                "● ",
                                Style::default()
                                    .fg(PENDING_ACCENT)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(text.clone(), style),
                        ]));
                    } else {
                        body_lines.push(RtLine::from(vec![Span::styled(text.clone(), style)]));
                    }
                }
                let tree_rows_body_end = body_lines.len();
                body_lines.push(RtLine::from(""));
                body_lines.push(RtLine::from(vec![Span::styled(
                    concept_status.clone(),
                    Style::default().fg(Color::LightMagenta),
                )]));
                body_lines.push(RtLine::from(""));
                body_lines.push(RtLine::from(vec![
                    Span::styled("key: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(last_key.clone().unwrap_or_else(|| "(none)".to_string())),
                ]));
                // Scroll the nav body so the selected tree row stays in
                // the comfort zone — the middle 1/3 of the pane. Going
                // down: once cursor crosses the bottom-third boundary,
                // the scroll advances so the cursor stays planted at
                // that boundary, no big jumps. Going up: same on the
                // top boundary. At the actual top/bottom of the body
                // the cursor falls through to the real first/last row,
                // since clamping `nav_scroll` to [0, max_scroll]
                // releases it. Header lines scroll off the top as a
                // simple trade; sub-paneled header/footer is a later
                // refinement.
                let nav_inner_h = nav_list_h;
                let body_len = body_lines.len();
                if !nav_has_cursor || body_len <= nav_inner_h {
                    nav_scroll = 0;
                } else {
                    let scrolloff = (nav_inner_h / 3).max(1);
                    let min_view = scrolloff;
                    // last comfort row in the viewport (inclusive)
                    let max_view = nav_inner_h.saturating_sub(scrolloff).saturating_sub(1);
                    let view_pos = nav_cursor_body_pos.saturating_sub(nav_scroll as usize);
                    if view_pos < min_view {
                        nav_scroll = (nav_cursor_body_pos.saturating_sub(min_view)) as u16;
                    } else if view_pos > max_view {
                        nav_scroll = (nav_cursor_body_pos.saturating_sub(max_view)) as u16;
                    }
                    let max_scroll = body_len.saturating_sub(nav_inner_h) as u16;
                    if nav_scroll > max_scroll {
                        nav_scroll = max_scroll;
                    }
                }
                // Nav-spill segment collection: for each visible TREE row
                // whose text is wider than the nav column, record the full
                // row (truncated to the overlay's reach cap) so the render-
                // pass tail can float it over the preview's left edge.
                // TREE rows only — the assembly-captured span above: header
                // and trailing chrome lines (status/help/concept/key) never
                // spill (codex review). Widths are terminal
                // CELLS via unicode-width, so CJK/emoji names measure and
                // truncate exactly (codex review).
                if nav_spill_active && nav_rect.width > 0 && preview_rect.width > 0 {
                    use unicode_width::UnicodeWidthStr;
                    // Reach: from the nav left edge to 2 cells short of the
                    // preview's right edge, in cells.
                    let max_cells = (preview_rect.x + preview_rect.width)
                        .saturating_sub(2)
                        .saturating_sub(nav_rect.x) as usize;
                    let first = nav_scroll as usize;
                    let tree_span = tree_rows_body_start..tree_rows_body_end;
                    let visible = body_lines.iter().skip(first).take(nav_list_h);
                    for (vis_idx, line) in visible.enumerate() {
                        if !tree_span.contains(&(first + vis_idx)) {
                            continue;
                        }
                        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                        let cell_w = UnicodeWidthStr::width(text.as_str());
                        let Some(take) = nav_spill_take(cell_w, nav_rect.width as usize, max_cells)
                        else {
                            continue;
                        };
                        // Style of the widest span — rows are one span, or
                        // sigil + text where the text span dominates (and
                        // the pending sigil shares the text's accent
                        // anyway), so a single-run overlay is colour-
                        // faithful in practice.
                        let style = line
                            .spans
                            .iter()
                            .max_by_key(|s| UnicodeWidthStr::width(s.content.as_ref()))
                            .map(|s| s.style)
                            .unwrap_or_default();
                        let (shown, shown_cells) = if take < cell_w {
                            truncate_to_cells(&text, take)
                        } else {
                            let w = UnicodeWidthStr::width(text.as_str());
                            (text, w)
                        };
                        let width_cells = shown_cells as u16;
                        nav_spill_segs_out.push(NavSpillSeg {
                            x: nav_rect.x,
                            row: nav_rect.y + vis_idx as u16,
                            text: shown,
                            width_cells,
                            color: crate::chrome::ratatui_color_to_rgb(style.fg),
                            bold: style.add_modifier.contains(Modifier::BOLD),
                            dim: style.add_modifier.contains(Modifier::DIM),
                        });
                    }
                }
                let nav_body = Paragraph::new(body_lines).scroll((nav_scroll, 0));

                // Other pane titles + body widgets. No Block / borders
                // — we paint the frame ourselves below so the math is
                // exact and there are no double walls.
                let preview_focus = focus == PaneFocus::Preview;
                let preview_pinned = self.pinned_preview_node_id.is_some();
                // T1: surface the full path of the file the preview is showing
                // (clipped in the narrow nav column) here in the wide title.
                // Markers go after the name so middle-truncating the name to
                // fit never drops [FOCUS]/[pinned *].
                let preview_title = {
                    let mut markers = String::new();
                    if preview_focus {
                        markers.push_str(" · [FOCUS]");
                    }
                    if preview_pinned {
                        markers.push_str(" · [pinned *]");
                    }
                    match preview_name.clone() {
                        Some(name) => {
                            // Budget the name against the pane width so even an
                            // over-long title keeps its basename + the markers.
                            let avail = preview_rect.width.saturating_sub(2) as usize;
                            let fixed = " preview · ".chars().count() + markers.chars().count() + 1; // trailing space
                            let name_budget = avail.saturating_sub(fixed).max(1);
                            let shown = middle_truncate(&name, name_budget);
                            format!(" preview · {shown}{markers} ")
                        }
                        None => format!(" preview{markers} "),
                    }
                };
                // The preview slot still needs its content cell rect
                // exported for the wgpu preview-layer surface.
                preview_cells = preview_rect;
                // Drawer cell rect, exported for the Ctrl+M monitor chart quad.
                repl_cells = repl_rect;
                // Cache the four pane content rects for between-frame
                // hit-testing (mouse wheel → which pane scrolls).
                new_pane_rects = PaneRects {
                    nav: nav_frame_rect,
                    preview: preview_rect,
                    llm: llm_rect,
                    repl: repl_rect,
                };

                let llm_focus = focus == PaneFocus::Llm;
                let llm_title = if llm_focus {
                    " llm · [FOCUS] ".to_string()
                } else {
                    " llm ".to_string()
                };

                let repl_focus = focus == PaneFocus::Repl;
                // G6: the drawer title reflects which content it's showing —
                // the Julia REPL (Ctrl+J) or the local terminal (Ctrl+T).
                let repl_title = match (drawer, repl_focus) {
                    (DrawerContent::Terminal, true) => " terminal · [FOCUS] ".to_string(),
                    (DrawerContent::Terminal, false) => " terminal ".to_string(),
                    // 4.3, option (a): names whose record this is — the
                    // resolved host (the hub when it's connected, else the
                    // `default_host` fallback), flagged when that host
                    // isn't actually among today's connections.
                    (DrawerContent::Monitor, _) => {
                        if monitor_host_connected {
                            format!(" monitor · {monitor_host_label} ")
                        } else {
                            format!(" monitor · {monitor_host_label} [not connected] ")
                        }
                    }
                    (DrawerContent::Help, _) => " help ".to_string(),
                    (_, true) => " repl · julia · [FOCUS] ".to_string(),
                    (_, false) => " repl · julia ".to_string(),
                };
                // Input pane height tracks the number of newline-separated
                // lines in `repl_input` so a multi-line buffer (built up
                // via Shift+Enter) is fully visible while editing. Capped
                // at `repl_rect.height - 1` so at least one row of
                // scrollback is always on screen — a runaway buffer
                // narrows scrollback but is still recoverable via Enter
                // or Backspace.
                let input_line_count = (repl_input.matches('\n').count() + 1) as u16;
                let max_input_rows = repl_rect.height.saturating_sub(1).max(1);
                let input_rows = input_line_count.min(max_input_rows).max(1);
                let repl_split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(input_rows)])
                    .split(repl_rect);
                let scroll_h = repl_split[0].height as usize;
                // Scrollback window: `repl_scroll` is the number of rows
                // *back from the tail*. 0 = live; positive = older. Clamp
                // so the user can't scroll past the top of the log, and
                // write the clamped value back to State so the wheel
                // handler doesn't accumulate dead range.
                let total = repl_lines.len();
                let max_scroll = total.saturating_sub(scroll_h) as u16;
                let clamped = new_repl_scroll.min(max_scroll);
                new_repl_scroll = clamped;
                let end = total.saturating_sub(clamped as usize);
                let start = end.saturating_sub(scroll_h);
                // Export for the inline-image paint pass: which absolute
                // lines are on screen, and the sub-rect they render into.
                repl_scrollback_cells = repl_split[0];
                repl_window = (start, end);
                let scroll_para = Paragraph::new(repl_lines[start..end].to_vec());
                // Mode-aware prompt: `julia> ` in cyan vs `pkg> ` in
                // blue (matches the standard Julia REPL palette). Dim
                // both when the REPL pane isn't focused — same
                // attention-direction trick as before.
                // Match the stdlib `REPL.jl` / VSCode Julia-ext palette:
                // `julia>` green, `pkg>` blue. Light* variants pop on the
                // near-black surface fg.
                let prompt_text = if repl_pkg_mode { "pkg> " } else { "julia> " };
                let prompt_focus_color = if repl_pkg_mode {
                    Color::LightBlue
                } else {
                    Color::LightGreen
                };
                let prompt_color = if repl_focus {
                    prompt_focus_color
                } else {
                    Color::DarkGray
                };
                // Multi-line input: first segment carries the live prompt,
                // continuation segments get a same-width filler so the
                // text column stays aligned under the prompt. Cursor
                // block lives at the end of the last segment regardless
                // of how many lines deep we are.
                let cont_pad: String = " ".repeat(prompt_text.len());
                let segments: Vec<&str> = repl_input.split('\n').collect();
                let last_idx = segments.len().saturating_sub(1);
                let input_rt_lines: Vec<RtLine> = segments
                    .iter()
                    .enumerate()
                    .map(|(i, seg)| {
                        let mut spans: Vec<Span> = Vec::with_capacity(3);
                        if i == 0 {
                            spans
                                .push(Span::styled(prompt_text, Style::default().fg(prompt_color)));
                        } else {
                            spans.push(Span::raw(cont_pad.clone()));
                        }
                        spans.push(Span::raw(seg.to_string()));
                        if repl_focus && i == last_idx {
                            spans.push(Span::styled(
                                "\u{2588}",
                                Style::default().fg(prompt_focus_color),
                            ));
                        }
                        RtLine::from(spans)
                    })
                    .collect();
                let input_para = Paragraph::new(input_rt_lines);

                // Render content widgets into the interior content
                // rects (no borders). The drawer's REPL scrollback + input
                // only render when the drawer is actually showing the REPL;
                // when it shows the Terminal (G3) the vt100 grid is painted
                // into `repl_rect` after the wireframe instead.
                frame.render_widget(nav_body, nav_list_rect);
                frame.render_widget(
                    Paragraph::new(
                        nav_pinned
                            .iter()
                            .map(|r| RtLine::from(Span::styled(r.clone(), Style::default().fg(Color::LightGreen))))
                            .collect::<Vec<_>>(),
                    ),
                    ratatui::layout::Rect {
                        y: nav_rect.y + nav_list_h as u16,
                        height: nav_pinned.len() as u16,
                        ..nav_rect
                    },
                );
                if drawer == DrawerContent::Help {
                    help::render(frame, repl_rect, help_state, help_bindings);
                }
                if drawer == DrawerContent::Repl {
                    frame.render_widget(scroll_para, repl_split[0]);
                    frame.render_widget(input_para, repl_split[1]);
                }

                // Paint the wireframe directly into the buffer.
                // vlines/hlines drive the inner borders + corner
                // junctions; outer perimeter is always drawn. Each
                // border cell is written exactly once.
                let buf = frame.buffer_mut();
                draw_wireframe(
                    buf,
                    area,
                    &geom.vlines,
                    &geom.hlines,
                    geom.drawer_x_end,
                    geom.llm_left_vline,
                    border_style,
                );
                // Titles overlay the top wireframe edge of each
                // column (and the drawer's top edge when open). Each
                // title is clamped to its column's interior width so
                // it can't smear over a divider or the neighbour's
                // title.
                let title_style_for = |focused: bool| {
                    if focused {
                        focus_title_style
                    } else {
                        idle_title_style
                    }
                };
                let title_w = |rect: ratatui::layout::Rect| rect.width.saturating_sub(2);
                if nav_frame_rect.width > 0 {
                    write_title(
                        buf,
                        nav_frame_rect.x + 1,
                        nav_frame_rect.y.saturating_sub(1),
                        &nav_title,
                        title_w(nav_frame_rect),
                        title_style_for(nav_focus),
                    );
                }
                if preview_rect.width > 0 {
                    write_title(
                        buf,
                        preview_rect.x + 1,
                        preview_rect.y.saturating_sub(1),
                        &preview_title,
                        title_w(preview_rect),
                        title_style_for(preview_focus),
                    );
                }
                if llm_rect.width > 0 {
                    write_title(
                        buf,
                        llm_rect.x + 1,
                        llm_rect.y.saturating_sub(1),
                        &llm_title,
                        title_w(llm_rect),
                        title_style_for(llm_focus),
                    );
                }
                if repl_rect.width > 0 {
                    write_title(
                        buf,
                        repl_rect.x + 1,
                        repl_rect.y.saturating_sub(1),
                        &repl_title,
                        title_w(repl_rect),
                        title_style_for(repl_focus),
                    );
                }

                let focused_rect = match focus {
                    PaneFocus::NavTree => nav_frame_rect, PaneFocus::Preview => preview_rect,
                    PaneFocus::Llm => llm_rect, PaneFocus::Repl => repl_rect,
                };
                if focused_rect.width > 2 {
                    let width = focused_rect.width.saturating_sub(2) as usize;
                    let title = format!(" {} · ", help_context.title());
                    let title_width = unicode_width::UnicodeWidthStr::width(title.as_str());
                    let hint = help::border(&help_context, help_bindings, width.saturating_sub(title_width));
                    let title = help::truncate(&format!("{title}{hint}"), width);
                    // Clear old title glyphs before writing the shorter dynamic title.
                    for x in focused_rect.x..focused_rect.x + focused_rect.width {
                        buf[(x, focused_rect.y.saturating_sub(1))].set_symbol("─").set_style(border_style);
                    }
                    write_title(buf, focused_rect.x + 1, focused_rect.y.saturating_sub(1),
                        &title, width as u16, focus_title_style);
                }

                // Live local-time clock, right-aligned on the top edge just
                // inside the outer-right corner glyph. Same chrome text style
                // as an idle pane title. Repaints ~1×/second via the
                // `about_to_wait` WaitUntil scheduling below.
                {
                    let clock_label = format!(" {} ", clock_label(clock_now, area.width));
                    let clock_cells = clock_label.chars().count() as u16;
                    // Keep the ┐ corner; sit one cell to its left, then back
                    // off by the label width. No-op if the window is too
                    // narrow to fit the clock without colliding with a title.
                    if area.width > clock_cells + 2 {
                        let clock_x = area.x + area.width - 1 - clock_cells;
                        write_title(
                            buf,
                            clock_x,
                            area.y,
                            &clock_label,
                            clock_cells,
                            idle_title_style,
                        );

                        // Battery indicator sits immediately left of the clock
                        // with a one-cell gap, same dim chrome style. Painted
                        // only if a battery is present (cached label is `Some`)
                        // AND the window is wide enough to fit it left of the
                        // clock without colliding with the left border. When
                        // it's too narrow we drop the battery and keep the
                        // clock.
                        if let Some(batt) = battery.as_deref() {
                            let batt_label = format!(" {batt} ");
                            let batt_cells = batt_label.chars().count() as u16;
                            // Need: left border (x) + at least one cell, then
                            // the battery, then the clock. Guard with the same
                            // ">" slack the clock uses.
                            if clock_x > area.x + batt_cells + 1 {
                                let batt_x = clock_x - batt_cells;
                                write_title(
                                    buf,
                                    batt_x,
                                    area.y,
                                    &batt_label,
                                    batt_cells,
                                    idle_title_style,
                                );
                            }
                        }
                    }
                }

                // FE/BE version stamp, left-aligned on the BOTTOM outer edge
                // — the mirror of the `nav · mode:` title on the top edge,
                // same `write_title` treatment and the same two-cell inset
                // from the corner glyph. Sits ON the border line; the session
                // strip is a pixel overlay one row lower, so the two don't
                // fight for the same cells.
                //
                // Dark gray when FE and BE agree, yellow when they don't:
                // the halves drift independently (rebuild one, forget the
                // other), and a skew you have to read character-by-character
                // to notice isn't surfaced at all.
                {
                    let stamp_cells = version_stamp.chars().count() as u16;
                    let bot_y = area.y + area.height - 1;
                    // Same guard shape as the clock: skip entirely rather
                    // than smear a truncated version across the corner when
                    // the window is too narrow to hold it.
                    if area.width > stamp_cells + 2 {
                        write_title(
                            buf,
                            area.x + 2,
                            bot_y,
                            &version_stamp,
                            stamp_cells,
                            if version_skew {
                                Style::default().fg(Color::Yellow)
                            } else {
                                idle_title_style
                            },
                        );
                    }
                }

                // LLM pane: paint the vt100 terminal grid into the
                // BL content rect. Walk every cell of the emulator
                // screen at (row, col), look up its glyph + colour,
                // and write into the chrome buffer at the matching
                // (llm_rect.x + col, llm_rect.y + row). The emulator
                // was sized to llm_rect earlier, so the grid fits
                // exactly.
                paint_terminal(buf, llm_rect, &pty_screen);
                // ADR 0030 §8 "Where it is shown", widened by ADR 0045
                // decision 1 (Codex review): overlays the persistent reason
                // line, and under it the discarded-input count, whenever
                // either is set —
                // whatever `pty_screen` actually painted underneath,
                // including a checkpointed client's own now-STALE frozen
                // content (a live failure/retry must never hide behind
                // real-but-old output), not only the dead-uncheckpointed
                // fallback to the (usually blank, unrelated) tmux screen
                // this originally covered.
                if llm_rect.width > 2 {
                    for (row, line) in pane_overlay.iter().enumerate().take(llm_rect.height as usize) {
                        write_title(
                            buf,
                            llm_rect.x + 1,
                            llm_rect.y + row as u16,
                            line,
                            llm_rect.width - 2,
                            Style::default().fg(Color::Yellow),
                        );
                    }
                }
                pty_size_observed = (llm_rect.width, llm_rect.height);
                // G3: local terminal drawer — paint its vt100 grid into the
                // drawer rect (same renderer as the LLM pane). Record the
                // rect so the PTY can be resized to match after the closure.
                if drawer == DrawerContent::Terminal && repl_rect.width > 0 {
                    if let Some(scr) = term_screen {
                        paint_terminal(buf, repl_rect, scr);
                    }
                    term_size_observed = (repl_rect.width, repl_rect.height);
                }
                // (The active-workspace indicator is now the bottom session
                // strip — all sessions, active centered + bold — drawn as a
                // pixel-positioned overlay after this ratatui pass via
                // `session_strip_lines`. It supersedes the old single
                // centered marker that used to paint here.)
            })
            .context("ratatui draw failed")?;
        // Persist the scroll the draw closure landed on so the next
        // frame starts from the same offset (sticky behaviour); the
        // closure can't write to self.tree_scroll directly because the
        // ratatui draw API takes a &mut self method, not self.
        self.tree_scroll = nav_scroll;
        self.repl_scroll = new_repl_scroll;
        self.pane_rects = new_pane_rects;
        self.nav_spill_segments = nav_spill_segs_out;

        // Track the LLM-pane's size once the first redraw has a real BL
        // content rect, and keep an already-live capsule client's
        // viewport in sync when the rect grows/shrinks — the actual
        // attach/open wire request is `attach_session_to_bl`'s, not
        // this redraw's.
        let (cols, rows) = pty_size_observed;
        if cols >= 2 && rows >= 2 {
            let need_open = self.pty_size.is_none();
            let need_resize = self
                .pty_size
                .map(|prev| prev != (cols, rows))
                .unwrap_or(false);
            // ADR 0042 slice L1b fix 2: `pane_feed`, not the 2-way
            // `pane_is_capsule` this used to branch on — `Pending` must
            // send NOTHING to either backend (a resize routed by
            // guesswork would be indistinguishable from the exact bug
            // finding 2 fixed for input) while still tracking the
            // latest size locally, since whichever backend eventually
            // resolves reads its start/resize from `self.pty_size`.
            match self.pane_feed {
                PaneFeed::Capsule => {
                    // The capsule client's connection is owned entirely
                    // by the `PtyAttachDirect` handler's own
                    // `spawn_pane_attach_term` call (the daemon's own
                    // reply to the `pty.open` `attach_session_to_bl`
                    // always sends) — there is no `pty.open`/`pty.resize` wire
                    // request for a capsule row (the daemon refuses both
                    // with `attach_direct`). This arm only keeps an
                    // ALREADY-LIVE client's viewport in sync with the pane
                    // rect, mirroring the drawer's own attach-client
                    // resize below (`term_size_observed`).
                    if need_open || need_resize {
                        if let Some(t) = self.pane_attach_term.as_mut() {
                            t.resize(cols, rows);
                            if need_resize {
                                // A resize reshapes the row map, so the old
                                // offset means nothing: snap to live.
                                t.screen_mut().set_scrollback(0);
                            }
                        }
                        self.pty_size = Some((cols, rows));
                    }
                }
                PaneFeed::Pending => {
                    if need_open || need_resize {
                        self.pty_size = Some((cols, rows));
                    }
                }
            }
        }

        // Resize the local terminal's PTY to the drawer rect once it's
        // known (G3). Spawned at a default 80x24; this snaps it to the
        // real drawer size on the first frame it's visible, and on any
        // later drawer geometry change.
        let (tcols, trows) = term_size_observed;
        if tcols >= 2 && trows >= 2 {
            let changed = self
                .term_size
                .map(|prev| prev != (tcols, trows))
                .unwrap_or(true);
            if changed {
                if let Some(t) = self.local_term.as_mut() {
                    t.resize(tcols, trows);
                }
                #[cfg(windows)]
                if let Some(t) = self.attach_term.as_mut() {
                    t.resize(tcols, trows);
                }
                self.term_size = Some((tcols, trows));
            }
        }

        let mut lines = self.terminal.backend().project_lines(
            self.chrome_origin_x,
            self.chrome_origin_y,
            self.cell_w,
            self.cell_h,
        );

        // Pane borders: render ratatui's box-drawing glyphs (│ ─ ┌ …) as
        // solid quads sized to the exact cell instead of font glyphs.
        // cosmic-text lays the generic monospace font's box glyphs inside the
        // leading-padded cell, so stacked `│` show sub-cell gaps; arm-from-
        // centre quads tile seamlessly by construction and are font-independent
        // (see chrome::project_border_quads). Same origin/scale as project_lines
        // above so the quads sit on the glyph grid exactly.
        //
        // Thickness ≈ 9% of cell height → a thin ~1–2px light-border weight that
        // scales with DPI (cell_h is BASE_CELL_H * scale).
        let border_thickness = border_thickness_px(self.cell_h);
        let border_quads_raw = self.terminal.backend().project_border_quads(
            self.chrome_origin_x,
            self.chrome_origin_y,
            self.cell_w,
            self.cell_h,
            border_thickness,
        );
        // Group rects by colour (1–2 colours typical): `Quad::render_many` is
        // one colour per Quad, so the pass below does one batched draw per
        // colour.
        let mut border_rects_by_color: HashMap<(u8, u8, u8), Vec<ScreenRect>> = HashMap::new();
        for bq in &border_quads_raw {
            border_rects_by_color
                .entry(bq.color)
                .or_default()
                .push(ScreenRect {
                    x: bq.x,
                    y: bq.y,
                    w: bq.w,
                    h: bq.h,
                });
        }
        // Ensure a cached 1×1 solid Quad exists for each colour BEFORE the
        // render pass — building inside the pass would need &mut
        // self.border_quads while the pass already holds other &self borrows.
        // Doing it here keeps the pass to pure iter_mut + render_many.
        for color in border_rects_by_color.keys() {
            if !self.border_quads.contains_key(color) {
                let (r, g, b) = *color;
                let quad = Quad::from_rgba8(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &[r, g, b, 255],
                    1,
                    1,
                )
                .context("failed to build border-colour quad")?;
                self.border_quads.insert(*color, quad);
            }
        }

        // Bottom session strip (floating overlay): all sessions laid out
        // horizontally, the active one centered + bold, neighbours dimmed to
        // either side. `strip_scroll_px` eases toward the active session's
        // strip-local center so a switch (Shift+←→ → cycle_workspace)
        // slides the strip macOS-style. Drawn here, before `extras` borrow
        // self, so the ease can mutate self.* without a borrow conflict.
        //
        // Each ship's bow wheel is accumulated here (physical px) and drawn
        // from `self.logo_quad` inside the render pass below — the per-ship
        // layout it needs only exists in this block.
        let mut strip_logo_rects: Vec<ScreenRect> = Vec::new();
        if !self.workspace_slugs.is_empty() {
            // ADR 0042 L2a: `workspace_slugs` is the UNION across every
            // host, so every parallel vector below keys off the full
            // `(host, slug)` pair, not the bare slug — two hosts can share
            // a slug, and the strip must not conflate their state.
            // Per-name badge-floor pending flag (ADR 0025 §1): true when that
            // workspace has a pending nav.preview result waiting, keyed by the
            // same (host, slug) WsKey pair pending_nav uses (ADR 0042 L2a codex
            // review, item E). Read BEFORE the labels because the badge is part
            // of the label (`strip_label`): every width below, the hull's
            // included, is measured from the text that is drawn.
            let pendings: Vec<bool> = self
                .workspace_slugs
                .iter()
                .map(|(h, s)| self.pending_nav.contains_key(&(h.clone(), s.clone())))
                .collect();
            let labels: Vec<String> = self
                .workspace_slugs
                .iter()
                .zip(&pendings)
                .map(|((h, s), &pending)| {
                    strip_label(
                        self.workspace_labels
                            .get(&(h.clone(), s.clone()))
                            .map(String::as_str)
                            .unwrap_or(s.as_str()),
                        pending,
                    )
                })
                .collect();
            let current_key: Option<WsKey> = self
                .active_workspace_id
                .clone()
                .or_else(|| self.default_workspace_slug.clone())
                .map(|s| (self.active_host.clone(), s));
            let active = current_key
                .as_ref()
                .and_then(|k| self.workspace_slugs.iter().position(|x| x == k))
                .unwrap_or(0)
                .min(labels.len().saturating_sub(1));
            // The fleet (owner asks 2026-09-07 + 2026-09-24 + 2026-09-27, ADR
            // 0042 L2a): each HOST GROUP is a ship — a brand wheel at the bow,
            // inline with the session names, and a row below it a waterline
            // running from the bow's rake to the stern with that group's box
            // name set into it, beneath the names it carries. The
            // name comes from `host_label`, the one display projection,
            // truncated like a session name is so a pathological host name
            // can't blow the layout ("local is just another host", so no
            // special-casing the group next to it). `logo_dims` doubles as
            // both "is there an asset to draw a wheel with" and the geometry
            // the layout reserves for it (one source for the logo's on-screen
            // size, not two); with no decoded logo the wheels drop out and the
            // names alone mark the groups, matching the asset's own
            // fail-soft contract (a decode failure never breaks the layout,
            // per `LOGO_DARK_PNG`'s doc).
            let logo_dims: Option<(f32, f32)> = self.logo_quad.as_ref().map(|(_, nw, nh)| {
                let logo_h = (self.cell_h - 2.0).max(1.0);
                let logo_w = logo_h * (*nw as f32 / (*nh).max(1) as f32);
                (logo_w, logo_h)
            });
            let items: Vec<StripItem> = strip_items(&self.workspace_slugs, |h| {
                strip_truncate(host_label(&self.declared_host, h))
            });
            // Every wheel is full size, and the layout reserves exactly that
            // and nothing more: which ship you steer is spent in the box name's
            // INK, not in its geometry, so switching ships still can't reflow
            // the strip — and the name itself is reserved by nobody, because it
            // runs a row BELOW the session names (`strip_item_widths`).
            let wheel_w = logo_dims.map(|(logo_w, _)| logo_w).unwrap_or(0.0);
            let label_widths: Vec<f32> = labels
                .iter()
                .map(|l| l.chars().count() as f32 * self.cell_w)
                .collect();
            let item_widths = strip_item_widths(&items, &label_widths, wheel_w);
            let item_positions = strip_cursor_positions(&item_widths, |i| {
                strip_gap_before(&items[i], self.cell_w)
            });
            let divider_offsets =
                strip_divider_offsets(&items, &item_widths, &label_widths, self.cell_w);
            let target = session_strip_target(&labels, active, self.cell_w, &divider_offsets);
            let scroll = self.strip_scroll_px.unwrap_or(target);
            // Two rows hanging off the bottom of the chrome grid: session
            // names above, the ships below them. The grid's own bottom edge is
            // where the chrome's last row was drawn (`project_lines` walks the
            // same rows from the same origin), and `cell_grid_for` has already
            // kept the band's rows out of it (`strip_reserved_rows`), so
            // neither row can land on the bottom border line and its version
            // stamp — and the air above the names is `STRIP_TOP_AIR_ROWS`
            // whatever the window height quantises to.
            let grid_bottom = self.chrome_origin_y
                + self.terminal.backend().rows() as f32 * self.cell_h;
            let (baseline_y, ship_y) = strip_row_tops(grid_bottom, self.cell_h);
            // Per-name work-state tone, parallel to `labels` (built from
            // `workspace_slugs` in the same order). `now` is fetched per frame
            // so the wilt re-evaluates on the existing 1 Hz idle redraw.
            let strip_now = chrono::Utc::now();
            let tones: Vec<Option<(AgentTone, bool)>> = self
                .workspace_slugs
                .iter()
                .map(|(h, s)| {
                    self.workspace_states
                        .get(&(h.clone(), s.clone()))
                        .and_then(|(st, at)| agent_tone_from(st, at, strip_now))
                })
                .collect();
            // Per-name status-change flash factor, parallel to `labels`.
            let flash_now = std::time::Instant::now();
            let flashes: Vec<f32> = self
                .workspace_slugs
                .iter()
                .map(|(h, s)| self.flash_factor_for(h, s, flash_now))
                .collect();
            let strip_lines = session_strip_lines(
                &labels,
                active,
                scroll,
                self.config.width as f32,
                self.cell_w,
                baseline_y,
                &tones,
                self.contrast_dim,
                &flashes,
                &pendings,
                &divider_offsets,
            );
            // ONE list of marks: every rect a ship draws — its wheel, its box
            // name, the waterline segments, the bow rake, the stern — culled
            // once, there (`ship_marks`, `strip_visible`), so what the loop
            // below paints is exactly what survived the cull and no draw site
            // can re-derive a second answer.
            let win_w = self.config.width as f32;
            let (_, logo_h) = logo_dims.unwrap_or((0.0, 0.0));
            let (hull_y, hull_h) = hull_band(ship_y, self.cell_h);
            let marks: Vec<StripMark> = ship_marks(
                &items,
                &item_positions,
                &item_widths,
                &self.active_host,
                |h| self.host_connected.get(h).copied().unwrap_or(false),
                wheel_w,
                self.cell_w,
                hull_h,
                scroll,
                win_w,
            );
            // Draw the marks. Every vertical coordinate comes from
            // `ship_vertical` — the one place the band's locked geometry lives.
            let vert = ship_vertical(baseline_y, ship_y, self.cell_h, logo_h);
            let mut strip_hull_rects: Vec<ScreenRect> = Vec::new();
            let mut tag_lines: Vec<crate::text::Line> = Vec::new();
            for m in &marks {
                match &m.kind {
                    StripMarkKind::Wheel => {
                        // Centred vertically in the SESSION-NAME row: the wheel
                        // is inline with the names (owner, 2026-09-27), which is
                        // what leaves the row below it free to be a line.
                        strip_logo_rects.push(ScreenRect {
                            x: m.left,
                            y: vert.wheel_y,
                            w: m.w,
                            h: logo_h,
                        });
                    }
                    StripMarkKind::BoxName { name, steered } => {
                        // Water blue, in its own tier — and the steered box in
                        // the lighter water (`box_name_rgb`). Never the cream a
                        // session name takes: identical ink made a host read as
                        // one more session. Never lifted, never bold, no tone,
                        // flash or badge sigil — a box name is chrome, and a
                        // host is not an agent.
                        tag_lines.push(crate::text::Line {
                            text: name.clone(),
                            x: m.left,
                            y: vert.name_y,
                            color: Some(box_name_rgb(*steered, self.contrast_dim)),
                            bold: false,
                            italic: false,
                            dim: false,
                        })
                    }
                    StripMarkKind::Hull => {
                        strip_hull_rects.extend(hull_bar_rect(m.left, m.left + m.w, hull_y, hull_h))
                    }
                    StripMarkKind::BowRake => strip_hull_rects.extend(hull_bow_rects(
                        m.left,
                        m.w,
                        hull_y,
                        hull_h,
                        vert.rake_rise,
                    )),
                    StripMarkKind::Stern => {
                        strip_hull_rects.extend(hull_stern_rect(m.left + m.w, hull_y, hull_h, ship_y))
                    }
                }
            }
            lines.extend(strip_lines);
            lines.extend(tag_lines);
            // Hulls ride the chrome's own colour-keyed solid-quad cache, so the
            // brown costs exactly one entry and one batched draw.
            if !strip_hull_rects.is_empty() {
                if !self.border_quads.contains_key(&HULL_RGB) {
                    let (r, g, b) = HULL_RGB;
                    let quad = Quad::from_rgba8(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &[r, g, b, 255],
                        1,
                        1,
                    )
                    .context("failed to build hull-colour quad")?;
                    self.border_quads.insert(HULL_RGB, quad);
                }
                border_rects_by_color
                    .entry(HULL_RGB)
                    .or_default()
                    .extend(strip_hull_rects);
            }
            // Ease toward `target` for the next frame; keep the frame loop
            // alive (dirty) until settled. Frame-rate-independent ease-out.
            let now = std::time::Instant::now();
            let dt = self
                .strip_anim_last
                .map(|t| (now - t).as_secs_f32().min(0.1))
                .unwrap_or(0.0);
            let k = if dt > 0.0 {
                1.0 - (-dt / STRIP_TAU).exp()
            } else {
                0.0
            };
            let next = scroll + (target - scroll) * k;
            if (target - next).abs() < 0.5 {
                self.strip_scroll_px = Some(target);
                self.strip_anim_last = None;
            } else {
                self.strip_scroll_px = Some(next);
                self.strip_anim_last = Some(now);
                self.dirty = true;
            }
            // Spin the brand wheels down on the same frame clock: advance the
            // angle by the current velocity, decay the velocity (frame-rate
            // independent), and keep the loop alive until it settles. The angle
            // is left wherever it stops — a wheel rests fine at any rotation.
            if self.wheel_vel.abs() > WHEEL_MIN_VEL {
                let wdt = self
                    .wheel_anim_last
                    .map(|t| (now - t).as_secs_f32().min(0.1))
                    .unwrap_or(0.0);
                self.wheel_angle += self.wheel_vel * wdt;
                self.wheel_vel *= (-wdt / WHEEL_TAU).exp();
                if self.wheel_vel.abs() <= WHEEL_MIN_VEL {
                    self.wheel_vel = 0.0;
                    self.wheel_anim_last = None;
                } else {
                    self.wheel_anim_last = Some(now);
                    self.dirty = true;
                }
            }
        }

        // Compute pixel rects from ratatui's cell rects, then letterbox each
        // image inside its rect.
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let ox = self.chrome_origin_x;
        let oy = self.chrome_origin_y;
        let cells_to_px = move |cells: ratatui::layout::Rect| ScreenRect {
            x: ox + cells.x as f32 * cell_w,
            y: oy + cells.y as f32 * cell_h,
            w: cells.width as f32 * cell_w,
            h: cells.height as f32 * cell_h,
        };
        // One pane rect for the file viewer. Priority cascade is just
        // PNG > markdown (or any text mime, rendered as markdown). The
        // concept annotation gets its own home once concept-mode-nav
        // lands — for now showing it here was overriding the actual
        // file content the user navigated to. SVG (math) also drops out
        // of the cascade by default; it comes back when inline math
        // placement is wired through the markdown buffer.
        let preview_rect = cells_to_px(preview_cells);
        // ADR 0030 §2: the protocol-mismatch overlay is a hard block — it takes
        // the preview pane over EVERYTHING (help included) until a clean
        // reconnect clears it. Rebuilt lazily below once md_rect_px is known.
        let show_fatal = self.protocol_mismatch.contains_key(&self.active_host);
        let show_png = self.preview_png.is_some() && !show_fatal;
        let show_svg = false;
        // Edit mode owns the preview pane: the file viewer hides so
        // the editable annotation body has the whole rect.
        let show_edit =
            !show_fatal && self.edit_state.is_some() && self.preview_edit.is_some();
        let show_md = !show_fatal && !show_png && !show_edit;

        // Figure caption (agent-supplied, ADR 0025): a band RESERVED at the
        // bottom of the preview pane. Computed here, before `png_rect`, because
        // every piece of image geometry downstream — letterbox fit, the zoom
        // ceiling, pan slack, the ROI solve and its inverse, the scissor — keys
        // off the pane rect, and reserving space only works if they all agree on
        // the SAME reduced rect. `image_rect` is that rect; `preview_rect`
        // continues to mean the whole pane for text/media/overlay draws.
        let caption_draw = if show_png {
            self.build_caption(preview_rect)
        } else {
            // No raster on screen (help, edit modal, markdown, fatal overlay):
            // nothing to caption, and the text pane keeps the full rect.
            self.caption_label = None;
            None
        };
        // Publish the band height BEFORE deriving image_rect, so the keyboard
        // zoom/pan handler (which runs outside the render pass) derives its pane
        // from the same number this frame drew with.
        self.caption_band_px = caption_draw.as_ref().map_or(0.0, |c| c.backing.h);
        let image_rect = image_rect_for_caption(preview_rect, self.caption_band_px);

        let png_rect = if show_png {
            self.preview_png
                .as_ref()
                .map(|q| letterbox(image_rect, q.size_px))
        } else {
            None
        };
        let svg_rect = if show_svg {
            self.preview_svg
                .as_ref()
                .map(|q| letterbox(preview_rect, q.size_px))
        } else {
            None
        };

        // Inset the markdown content from the pane edge so text isn't flush
        // against the border — GitHub (`.markdown-body` padding) and VSCode
        // (~26px body padding) both gutter their rendered markdown. We translate
        // that to cell units: ~1 char each side + a half-line top/bottom. Tune
        // PREVIEW_PAD_X/Y to taste. Images keep the full `preview_rect` (the PNG
        // path above letterboxes into it) — only flowed text gets the gutter.
        const PREVIEW_PAD_X: f32 = 1.0; // cells, each side
        const PREVIEW_PAD_Y: f32 = 0.5; // cells, top & bottom
        let pad_x = PREVIEW_PAD_X * cell_w;
        let pad_y = PREVIEW_PAD_Y * cell_h;
        // Re-shape the markdown buffer if the pane rect changed shape.
        let md_rect = ScreenRect {
            x: preview_rect.x + pad_x,
            y: preview_rect.y + pad_y,
            w: (preview_rect.w - 2.0 * pad_x).max(1.0),
            h: (preview_rect.h - 2.0 * pad_y).max(1.0),
        };
        let size_changed = (md_rect.w - self.md_rect_px.w).abs() > 0.5
            || (md_rect.h - self.md_rect_px.h).abs() > 0.5;
        if size_changed {
            self.preview_md
                .resize(self.text.font_system_mut(), md_rect.w, md_rect.h);
        }
        self.md_rect_px = md_rect;
        // Ctrl+M monitor drawer (ADR 0020): the chart shares the drawer rect.
        // Regenerate the SVG → wgpu quad whenever data or size changed
        // (`monitor_dirty`), mirroring the math-SVG rasterise path. Build into
        // a local first so the immutable `&self.device/queue/quad_pipeline`
        // borrows don't collide with the `self.monitor_quad` write.
        self.repl_scrollback_px = cells_to_px(repl_scrollback_cells);
        self.repl_window = repl_window;
        self.monitor_rect_px = cells_to_px(repl_cells);
        if self.drawer == DrawerContent::Monitor {
            let mw = self.monitor_rect_px.w.max(1.0) as u32;
            let mh = self.monitor_rect_px.h.max(1.0) as u32;
            if self.monitor_dirty && mw > 1 && mh > 1 {
                // Scale the chart's text + gutters to match the chrome's
                // effective text size (cell_h is BASE_CELL_H * scale), so the
                // SVG's logical-px labels aren't tiny on a hi-DPI window.
                let mon_scale = (self.cell_h / BASE_CELL_H) as f64;
                let svg = self.monitor_view.render_svg(mw, mh, mon_scale);
                let quad = quad_from_svg_bytes(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    svg.as_bytes(),
                    mw,
                    mh,
                )
                .ok();
                self.monitor_quad = quad;
                self.monitor_dirty = false;
            }
        } else {
            self.monitor_quad = None;
        }
        // ADR 0030 §2: same lazy build for the protocol-mismatch overlay, once
        // md_rect_px reflects the real preview width so the message wraps right.
        if show_fatal && self.preview_fatal.is_none() {
            self.rebuild_fatal_overlay();
        }

        // Same dance for the concept pane. Shares the same rect now that
        // it's a single preview slot.
        let concept_rect = preview_rect;
        let concept_size_changed = (concept_rect.w - self.concept_rect_px.w).abs() > 0.5
            || (concept_rect.h - self.concept_rect_px.h).abs() > 0.5;
        if concept_size_changed {
            if let Some(pc) = self.preview_concept.as_mut() {
                pc.resize(self.text.font_system_mut(), concept_rect.w, concept_rect.h);
            }
        }
        self.concept_rect_px = concept_rect;

        // Clamp `preview_scroll` so the user can't walk past the end
        // of the document. Pixel-summing per LayoutLine accounts for
        // per-line `line_height_opt` overrides emitted by tall
        // placeholder spans (display math, embedded figures) — using
        // a body-line count alone undercounts the document height by
        // (figure_height - body_line_h) for every embedded media row.
        // Clamp by the one buffer on screen, in the rect it is drawn in, so a
        // hidden buffer never scrolls the pane into blank space.
        let line_h = self.preview_md.line_height().max(1.0);
        // The extras paint with `EXTRA_TOP_PAD_PX` of headroom, so each
        // frame only renders `the drawn rect's height - pad` pixels of content. Subtract
        // the pad from `visible_px` so max_scroll lets the user reach the
        // actual bottom of the document without losing the tail to the
        // padding.
        let (shown, shown_h) = preview_scroll_target(
            show_edit,
            &self.preview_md,
            md_rect.h,
            self.preview_edit.as_ref(),
            preview_rect.h,
        );
        let visible_px = (shown_h - crate::text::EXTRA_TOP_PAD_PX).max(line_h);
        // `preview_scroll` is body-line units; convert the pixel slack
        // back via ceil so the final body-line step always lands the
        // bottom of the document on screen (no off-by-fraction clip).
        let max_scroll = preview_max_scroll(line_h, visible_px, shown);
        self.preview_scroll = self.preview_scroll.min(max_scroll);
        let preview_scroll_px = self.preview_scroll as f32 * line_h;

        // Walk the laid-out markdown buffer for FFFC placeholders, zip
        // with `preview_md.media_blocks` by appearance order, and
        // pre-rasterise any math SVGs we have that haven't been
        // rasterised yet at the current pane width. Painting happens
        // inside the rpass below; do the side-effecty rasterise here
        // while we have &mut self.
        let media_paint_targets: Vec<(usize, ScreenRect)> = if show_md {
            self.collect_media_paint_targets(md_rect, preview_scroll_px)
        } else {
            Vec::new()
        };
        // Build / refresh per-table cosmic-text buffers — must run
        // before the extras are assembled below because the extras
        // borrow `&self.table_buffers[i].buffer`. The fn is a no-op
        // when the buffer set already matches `preview_md.media_blocks`
        // (typical steady-state across redraws). It also resets
        // `md_table_scroll_px` to 0 whenever buffers are rebuilt, so
        // navigating to a different doc starts at scroll-left.
        if show_md {
            self.ensure_table_buffers();
        } else {
            self.table_buffers.clear();
        }
        // Clamp the shared horizontal scroll so the user can't walk
        // past the right edge of the widest table on the current doc.
        // Uses the widest natural width across all tables — single
        // scroll var means the clamp has to cover them all (the
        // narrower tables just go past their own right edge into
        // empty space, which TextBounds clips invisibly).
        let widest_table_w = self
            .table_buffers
            .iter()
            .map(|e| e.natural_w_px)
            .fold(0.0_f32, f32::max);
        let table_max_scroll = (widest_table_w - md_rect.w).max(0.0);
        self.md_table_scroll_px = self.md_table_scroll_px.clamp(0.0, table_max_scroll);
        let body_em_px = self.preview_md.body_em().max(1.0);
        for (block_idx, rect) in &media_paint_targets {
            let Some(block) = self.preview_md.media_blocks.get(*block_idx) else {
                continue;
            };
            let crate::preview::markdown::MediaBlock::Math { latex, display } = block else {
                continue;
            };
            let key = (latex.clone(), *display);
            let Some(entry) = self.math_cache.get_mut(&key) else {
                continue;
            };
            if entry.rasterised.is_some() {
                continue;
            }
            // Natural pixel size, derived from the SVG's ex-unit
            // dimensions and the body font. Clamped to the row
            // reservation so a malformed `<svg>` tag (or an
            // exceptionally tall `aligned` block) can't overflow the
            // letterbox and trample neighbouring paragraphs. The fit-
            // scale used to happen inside `quad_from_svg_bytes`; doing
            // it here lets short equations rasterise at their actual
            // size (e.g. ~3ex tall) instead of being stretched to fill
            // the slab.
            let (target_w, target_h) = match (entry.width_ex, entry.height_ex) {
                (Some(w_ex), Some(h_ex)) => {
                    let nat_w = (w_ex * MATHJAX_EX_FACTOR * body_em_px).max(1.0);
                    let nat_h = (h_ex * MATHJAX_EX_FACTOR * body_em_px).max(1.0);
                    let max_w = rect.w.max(1.0);
                    let max_h = rect.h.max(1.0);
                    // Uniform downscale only — never enlarge past natural.
                    let s = (max_w / nat_w).min(max_h / nat_h).min(1.0).max(0.001);
                    (
                        (nat_w * s).ceil().max(1.0) as u32,
                        (nat_h * s).ceil().max(1.0) as u32,
                    )
                }
                _ => {
                    // Pre-fix fallback: no parsed dims, letterbox into
                    // the row reservation. Should be rare — the SVG's
                    // root tag is well-formed in every observed case.
                    ((rect.w as u32).max(1), (rect.h as u32).max(1))
                }
            };
            match quad_from_svg_bytes(
                &self.device,
                &self.queue,
                &self.quad_pipeline,
                &entry.svg_bytes,
                target_w,
                target_h,
            ) {
                Ok(q) => entry.rasterised = Some(q),
                Err(e) => tracing::warn!(error = %e,
                    latex_len = entry.svg_bytes.len(),
                    "math svg rasterise failed"),
            }
        }

        // ADR 0034: compute the scalebar geometry + shape its label BEFORE the
        // `extras` borrows and `text.prepare` — the label is pushed as an
        // ExtraArea below, and the bar rects are drawn inside the pass. `&mut
        // self` here (font_system + scalebar_label); done before the shared
        // buffer borrows the `extras` Vec takes.
        // `image_rect`, not `preview_rect`: the bar belongs to the figure, so
        // when a caption reserves the bottom band the bar rides above it with
        // no inset arithmetic of its own.
        let scalebar_draw = self.build_scalebar(png_rect, image_rect);

        // The preview text is laid out at its own line pitch, so the cell-grid
        // bottom rarely lands on a line boundary; clip at the last whole line.
        let whole_line_clip = |p: &MarkdownPreview, r: ScreenRect, scroll_px: f32| {
            r.y + crate::text::EXTRA_TOP_PAD_PX
                + p.whole_line_bottom(scroll_px, r.h - crate::text::EXTRA_TOP_PAD_PX)
        };
        let md_clip_bottom = whole_line_clip(&self.preview_md, md_rect, preview_scroll_px);
        let mut extras: Vec<crate::text::ExtraArea> = Vec::new();
        if let (Some(sb), Some(lbl)) = (scalebar_draw.as_ref(), self.scalebar_label.as_ref()) {
            extras.push(crate::text::ExtraArea {
                buffer: &lbl.buffer,
                x: sb.label_x,
                y: sb.label_y,
                right: image_rect.x + image_rect.w,
                bottom: image_rect.y + image_rect.h,
                clip_left: Some(image_rect.x),
                clip_top: Some(image_rect.y),
                // Near-white on the dark backing box drawn under it.
                color: (245, 245, 245),
                scroll_y_px: 0.0,
            });
        }
        if let (Some(cap), Some(lbl)) = (caption_draw.as_ref(), self.caption_label.as_ref()) {
            extras.push(crate::text::ExtraArea {
                buffer: &lbl.buffer,
                x: cap.text_x,
                y: cap.text_y,
                right: cap.backing.x + cap.backing.w,
                // `clip_bottom` (not the backing's edge) so a caption longer
                // than CAPTION_MAX_LINES is cut at the band's text area instead
                // of bleeding into the padding.
                bottom: cap.clip_bottom,
                clip_left: Some(cap.backing.x),
                clip_top: Some(cap.backing.y),
                color: (232, 232, 232),
                scroll_y_px: 0.0,
            });
        }
        if show_md {
            extras.push(crate::text::ExtraArea {
                buffer: &self.preview_md.buffer,
                x: md_rect.x,
                y: md_rect.y,
                right: md_rect.x + md_rect.w,
                bottom: md_clip_bottom,
                clip_left: None,
                clip_top: None,
                color: (220, 220, 220),
                scroll_y_px: preview_scroll_px,
            });
        }
        if show_edit {
            if let Some(pe) = self.preview_edit.as_ref() {
                extras.push(crate::text::ExtraArea {
                    buffer: &pe.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_line_clip(pe, preview_rect, preview_scroll_px),
                    clip_left: None,
                    clip_top: None,
                    // Warm gold tint so the user sees at a glance that
                    // this is editable, not the read-only annotation.
                    color: (235, 215, 160),
                    scroll_y_px: preview_scroll_px,
                });
            }
        }
        // ADR 0030 §2: protocol-mismatch overlay, reusing the help overlay's
        // paint path but with a warm red tint so it reads as an error, not a
        // cheat sheet. Trumps everything (highest priority in the cascade).
        if show_fatal {
            if let Some(pf) = self.preview_fatal.as_ref() {
                extras.push(crate::text::ExtraArea {
                    buffer: &pf.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_line_clip(pf, preview_rect, 0.0),
                    clip_left: None,
                    clip_top: None,
                    color: (240, 160, 150),
                    scroll_y_px: 0.0,
                });
            }
        }
        // Per-table extras — one ExtraArea per MediaBlock::Table, hosted
        // at the FFFC's screen rect with a left-shift of
        // `md_table_scroll_px` so the user can drag the table
        // horizontally. TextBounds at preview-pane edges clip the
        // overflow glyph-by-glyph — no wgpu scissor needed.
        //
        // We iterate `media_paint_targets` (in FFFC source order) and
        // increment a `table_idx` counter on each Table encounter so it
        // walks `table_buffers` parallel to the source-order TableBufferEntry
        // build inside `ensure_table_buffers`.
        if show_md && !self.table_buffers.is_empty() {
            let mut table_idx: usize = 0;
            for (block_idx, rect) in &media_paint_targets {
                let Some(block) = self.preview_md.media_blocks.get(*block_idx) else {
                    continue;
                };
                if !matches!(block, crate::preview::markdown::MediaBlock::Table { .. }) {
                    continue;
                }
                let Some(entry) = self.table_buffers.get(table_idx) else {
                    table_idx += 1;
                    continue;
                };
                table_idx += 1;
                // Cull tables fully scrolled off the vertical viewport
                // — TextBounds would catch them anyway but the cheap
                // skip saves a glyphon TextArea entry.
                if rect.y + rect.h < preview_rect.y || rect.y > preview_rect.y + preview_rect.h {
                    continue;
                }
                // `x` rides the shared horizontal scroll; `y` plants
                // the table's first row exactly at the FFFC's screen
                // y. The ExtraArea pipeline adds EXTRA_TOP_PAD_PX to
                // the y, so we subtract it back here.
                let table_x = rect.x - self.md_table_scroll_px;
                let table_y = rect.y - crate::text::EXTRA_TOP_PAD_PX;
                extras.push(crate::text::ExtraArea {
                    buffer: &entry.buffer,
                    x: table_x,
                    y: table_y,
                    // Bounds clip to the preview pane in BOTH axes so
                    // the table's natural-width overflow gets glyph-
                    // clipped at the pane right edge, and vertical
                    // scroll past the pane edges is invisible. The bottom
                    // stops at the last whole row.
                    right: preview_rect.x + preview_rect.w,
                    bottom: whole_row_bottom(
                        rect.y,
                        entry.buffer.metrics().line_height,
                        preview_rect.y + preview_rect.h,
                    ),
                    // Pin the bounds.left to the pane edge — the
                    // glyph origin (`x`) is shifted into negative
                    // territory by `md_table_scroll_px` and would
                    // otherwise let bounds.left follow it off-pane.
                    clip_left: Some(preview_rect.x),
                    clip_top: Some(preview_rect.y),
                    color: (220, 220, 220),
                    scroll_y_px: 0.0,
                });
            }
        }

        self.text.prepare(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &lines,
            &extras,
        )?;

        let mut help_overlay_rect = None;
        let mut help_overlay_lines = Vec::new();
        let mut help_opacity = 1.0;
        if let Some(peek) = self.help.peek.clone() {
            let now = std::time::Instant::now();
            help_opacity = peek.opacity(now);
            if help_opacity <= 0.0 || peek.context != self.help_context() {
                self.help.peek = None;
            } else {
                let rect = match peek.context.pane {
                    help::Pane::Nav => self.pane_rects.nav,
                    help::Pane::Preview => self.pane_rects.preview,
                    help::Pane::Agent => self.pane_rects.llm,
                    _ => self.pane_rects.repl,
                };
                let content = help::peek_lines(&peek.context, &self.bindings);
                let text_width = rect.width.saturating_sub(4) as usize;
                let longest = content.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.as_str())).max().unwrap_or(0);
                if text_width < longest || rect.height < content.len() as u16 + 2 {
                    self.open_help_drawer(peek.context);
                } else {
                    self.nav_spill_segments.clear();
                    let px = ScreenRect {
                        x: self.chrome_origin_x + rect.x as f32 * self.cell_w,
                        y: self.chrome_origin_y + rect.y as f32 * self.cell_h,
                        w: rect.width as f32 * self.cell_w,
                        h: (content.len() as f32 + 2.0) * self.cell_h,
                    };
                    help_overlay_rect = Some(px);
                    help_overlay_lines = content.into_iter().enumerate().map(|(i, text)| crate::text::Line {
                        text, x: px.x + 2.0 * self.cell_w, y: px.y + (i as f32 + 1.0) * self.cell_h,
                        color: Some((167, 222, 231)), bold: i == 0, italic: false, dim: false,
                    }).collect();
                    let alpha = (help_opacity * 245.0).round() as u8;
                    if self.help_back_quad.as_ref().map(|(a, _)| *a) != Some(alpha) {
                        self.help_back_quad = Some((alpha, Quad::from_rgba8(&self.device, &self.queue,
                            &self.quad_pipeline, &[14, 30, 46, alpha], 1, 1)?));
                    }
                }
            }
        }

        // Nav-spill overlay text: the segments the draw closure just
        // collected, converted cell→px with the SAME origin/cell math the
        // chrome lines use so the overlay realigns pixel-identically over
        // the row it covers. Prepared EVERY frame — an empty list is what
        // clears the overlay renderer's retained geometry (see
        // `prepare_overlay`'s doc), so no `if` around this call.
        let mut overlay_lines: Vec<crate::text::Line> = self
            .nav_spill_segments
            .iter()
            .map(|seg| crate::text::Line {
                text: seg.text.clone(),
                x: self.chrome_origin_x + seg.x as f32 * self.cell_w,
                y: self.chrome_origin_y + seg.row as f32 * self.cell_h,
                color: seg.color,
                bold: seg.bold,
                italic: false,
                dim: seg.dim,
            })
            .collect();
        let fade_start = overlay_lines.len();
        overlay_lines.extend(help_overlay_lines);
        self.text.prepare_overlay(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &overlay_lines,
            Some((fade_start, help_opacity)),
        )?;

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface.get_current_texture()?
            }
            Err(e) => return Err(e.into()),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sot-frame"),
            });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear+preview+text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            // Preview-layer goes under chrome text so borders and labels stay
            // legible above whatever the preview is. PNG uses a canvas
            // model — at zoom 1 the canvas equals letterbox (image inside
            // pane, aspect preserved); at zoom > 1 the canvas grows by
            // `zoom` and is rendered at full size with a scissor clip to
            // the pane, so the zoomed-in view fills the whole pane.
            // ADR 0022: recomputed below when an image is shown; cleared each
            // frame so a switch to markdown / no-preview drops the stale ROI.
            self.preview_roi = None;
            if let (Some(quad), Some(letterbox_rect)) = (self.preview_png.as_ref(), png_rect) {
                // Re-clamp against the live pane size before sizing the
                // canvas: a pane resize or a zoom restored from the
                // view-state cache can sit above the per-pixel ceiling for
                // the current geometry, and the canvas must honour it. Same
                // `(image_rect, size_px)` inputs as the `letterbox` above —
                // they must stay in lockstep, caption band included, or at the
                // ceiling canvas_w no longer equals 16 × source-px exactly.
                let zoom_max = png_zoom_max(image_rect.w, image_rect.h, quad.size_px);
                // ADR 0025 `preview --roi`: consume a pending viewport aim
                // whose image is the installed quad (ready — certified at
                // preview install) AND still the fired preview target. The
                // solve overrides zoom/pan here, where the live pane geometry
                // exists; the ordinary clamps just below then produce the
                // effective rect echoed back after `preview_roi` recomputes.
                let mut roi_aim: Option<RoiAim> = None;
                if self.pending_roi_aim.as_ref().is_some_and(|a| {
                    a.ready && Some(a.node_id.as_str()) == self.preview_node_id_fired.as_deref()
                }) {
                    let aim = self.pending_roi_aim.take().expect("checked Some above");
                    let (src_w, src_h) = self.preview_png_dims.unwrap_or(quad.size_px);
                    match solve_roi_view(
                        image_rect.w,
                        image_rect.h,
                        letterbox_rect.w,
                        letterbox_rect.h,
                        zoom_max,
                        src_w,
                        src_h,
                        aim.rect,
                    ) {
                        Some((z, pan)) => {
                            self.preview_png_zoom = z;
                            self.preview_png_pan_px = pan;
                            roi_aim = Some(aim);
                        }
                        None => tracing::warn!(node_id = %aim.node_id,
                            "preview --roi: degenerate geometry — aim dropped"),
                    }
                }
                if roi_aim.is_some() {
                    // An explicit `--roi` aim beats the view carry: both
                    // target this install, and the aim is a user/CLI ask.
                    self.pending_roi_restore = None;
                } else if self.pending_roi_restore.as_ref().is_some_and(|(nid, _)| {
                    Some(nid.as_str()) == self.preview_node_id_fired.as_deref()
                }) {
                    // Same-dir same-size view carry (`preview_png_cache`),
                    // deferred from preview install to here — the first
                    // frame with this node's OWN caption band in
                    // `image_rect`. Solved exactly like an aim; the
                    // ordinary clamps below still apply. Deliberately no
                    // `preview_roi_applied` echo: that event is the
                    // ADR-0025 contract for explicit aims only.
                    let (nid, rect) = self.pending_roi_restore.take().expect("checked Some above");
                    let (src_w, src_h) = self.preview_png_dims.unwrap_or(quad.size_px);
                    match solve_roi_view(
                        image_rect.w,
                        image_rect.h,
                        letterbox_rect.w,
                        letterbox_rect.h,
                        zoom_max,
                        src_w,
                        src_h,
                        rect,
                    ) {
                        Some((z, pan)) => {
                            self.preview_png_zoom = z;
                            self.preview_png_pan_px = pan;
                        }
                        // Degraded, not broken — the view stays at fit. Logged
                        // (debug, not the aim path's warn: no user asked for
                        // this rect) so a mysteriously-not-carried view is
                        // diagnosable.
                        None => tracing::debug!(node_id = %nid,
                            "view carry: degenerate geometry — restore dropped"),
                    }
                }
                let zoom = self.preview_png_zoom.clamp(1.0, zoom_max);
                self.preview_png_zoom = zoom;
                let canvas_w = letterbox_rect.w * zoom;
                let canvas_h = letterbox_rect.h * zoom;
                let pane_cx = image_rect.x + image_rect.w * 0.5;
                let pane_cy = image_rect.y + image_rect.h * 0.5;
                // Clamp pan so canvas always covers the pane in any axis
                // where canvas > pane. When canvas < pane (e.g. at zoom
                // 1 with a non-pane-aspect image), pan in that axis is
                // forced to 0 so the letterbox stays centred.
                let slack_x = (canvas_w - image_rect.w).max(0.0);
                let slack_y = (canvas_h - image_rect.h).max(0.0);
                let pan_x = self
                    .preview_png_pan_px
                    .0
                    .clamp(-slack_x * 0.5, slack_x * 0.5);
                let pan_y = self
                    .preview_png_pan_px
                    .1
                    .clamp(-slack_y * 0.5, slack_y * 0.5);
                self.preview_png_pan_px = (pan_x, pan_y);
                let canvas_rect = ScreenRect {
                    x: pane_cx - canvas_w * 0.5 + pan_x,
                    y: pane_cy - canvas_h * 0.5 + pan_y,
                    w: canvas_w,
                    h: canvas_h,
                };
                // ADR 0022: stash the visible ROI in source-image px so the
                // `C` hotkey / `capture_roi` fe-command and fe-state.json know
                // what's on screen. Image files only — a PDF page's source is
                // the `.pdf`, which `image.crop` can't decode (v2). Computed
                // into a local (shared borrows) then field-assigned, so it
                // doesn't fight `quad`'s borrow of `self.preview_png`.
                let new_roi: Option<PreviewRoi> =
                    self.preview_node_id_fired.as_ref().and_then(|nid| {
                        if !Self::is_image_node_id(nid) {
                            return None;
                        }
                        let (src_w, src_h) = self.preview_png_dims.unwrap_or(quad.size_px);
                        let (x, y, w, h) = visible_roi_px(
                            canvas_rect.x,
                            canvas_rect.y,
                            canvas_w,
                            canvas_h,
                            image_rect.x,
                            image_rect.y,
                            image_rect.w,
                            image_rect.h,
                            src_w,
                            src_h,
                        )?;
                        Some(PreviewRoi {
                            node_id: nid.clone(),
                            path: self.backend_abs_path(nid),
                            x,
                            y,
                            w,
                            h,
                            src_w,
                            src_h,
                            zoom,
                        })
                    });
                self.preview_roi = new_roi;
                // Write-through view carry: persist the freshly computed
                // visible ROI so same-dir same-size neighbors restore this
                // view (`preview_png_cache`). Done here, not at keystroke
                // time, because only this pass has the post-clamp geometry.
                // The hysteresis matters: a restore's own readback lands
                // within quantization (±1 px/edge) of the rect it restored,
                // and overwriting with it would ratchet — each flip
                // re-fitting a rect one pixel bigger, the view creeping out.
                // Keeping the incumbent inside that window makes the carry a
                // true fixed point; real zoom/pan input moves edges by far
                // more than a pixel, so nothing a user does is swallowed.
                // `preview_roi` is image-node-gated, so PDF pages never save.
                if let Some(roi) = self.preview_roi.as_ref() {
                    if let Some(key) = png_cache_key_from_node_id(
                        Some(roi.node_id.as_str()),
                        (roi.src_w, roi.src_h),
                    ) {
                        let rect = RoiRect {
                            x: roi.x,
                            y: roi.y,
                            w: roi.w,
                            h: roi.h,
                        };
                        match self.preview_png_cache.get(&key) {
                            Some(prev) if roi_rects_within_quantization(*prev, rect) => {}
                            _ => {
                                self.preview_png_cache.insert(key, rect);
                            }
                        }
                    }
                }
                // ADR 0025: echo the effective (post-clamp) rect for a just-
                // applied `--roi` aim — `fe.command.send` is fire-and-forget,
                // so the ack couldn't carry it (2026-07-21 ADR update).
                if let Some(aim) = roi_aim {
                    match self.preview_roi.as_ref() {
                        Some(eff) => self.emit_preview_roi_applied(&aim, eff),
                        // Raster but not a croppable image node (e.g. a PDF
                        // page): no source-px frame to report against.
                        None => tracing::warn!(node_id = %aim.node_id,
                            "preview --roi: no source-px ROI for this preview — no roi_applied echo"),
                    }
                }
                // Scissor to the IMAGE rect, not the pane: this is what stops a
                // zoomed/panned canvas from painting over the reserved caption
                // band at the bottom.
                let sx = image_rect.x.max(0.0) as u32;
                let sy = image_rect.y.max(0.0) as u32;
                let sw = image_rect.w.max(0.0) as u32;
                let sh = image_rect.h.max(0.0) as u32;
                rpass.set_scissor_rect(sx, sy, sw, sh);
                quad.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    canvas_rect,
                    (self.config.width, self.config.height),
                )?;
                // Reset scissor so subsequent draws (SVG, media paint,
                // chrome text) aren't clipped to the preview pane.
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }
            // ADR 0034: dynamic scalebar overlay, drawn after the image quad so
            // it sits on top, re-scissored to the pane so it can't bleed. Black
            // backing box under a white bar (the label rides in `extras`).
            // Dedicated quad fields, not the shared `border_quads` map: each
            // `render_many` mutably borrows its quad for the whole render-pass
            // lifetime (`'a`), so two colours must come from two disjoint
            // fields — one map borrowed twice would alias. `self.preview_png`'s
            // borrow ended when the image block closed above.
            if let Some(sb) = scalebar_draw.as_ref() {
                let sx = image_rect.x.max(0.0) as u32;
                let sy = image_rect.y.max(0.0) as u32;
                let sw = image_rect.w.max(0.0) as u32;
                let sh = image_rect.h.max(0.0) as u32;
                rpass.set_scissor_rect(sx, sy, sw, sh);
                self.scalebar_back_quad.render_many(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    std::slice::from_ref(&sb.backing),
                    (self.config.width, self.config.height),
                )?;
                self.scalebar_bar_quad.render_many(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    std::slice::from_ref(&sb.bar),
                    (self.config.width, self.config.height),
                )?;
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }
            // Figure-caption band. Its own quad field for the aliasing reason
            // documented on the scalebar above. Scissored to the FULL pane
            // (`preview_rect`) — the band lives in the strip `image_rect`
            // deliberately gave up, which is outside the image scissor.
            if let Some(cap) = caption_draw.as_ref() {
                let sx = preview_rect.x.max(0.0) as u32;
                let sy = preview_rect.y.max(0.0) as u32;
                let sw = preview_rect.w.max(0.0) as u32;
                let sh = preview_rect.h.max(0.0) as u32;
                rpass.set_scissor_rect(sx, sy, sw, sh);
                self.caption_back_quad.render_many(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    std::slice::from_ref(&cap.backing),
                    (self.config.width, self.config.height),
                )?;
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }
            if let (Some(quad), Some(rect)) = (self.preview_svg.as_ref(), svg_rect) {
                quad.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    rect,
                    (self.config.width, self.config.height),
                )?;
            }
            // Ctrl+M monitor chart: paint the rasterised SVG quad over the
            // drawer rect (ADR 0020), reusing the same resvg→wgpu-quad path as
            // the math SVG above.
            if self.drawer == DrawerContent::Monitor {
                if let Some(q) = self.monitor_quad.as_ref() {
                    q.render(
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        self.monitor_rect_px,
                        (self.config.width, self.config.height),
                    )?;
                }
            }
            // Inline REPL figures: paint each visible slot's quad over its
            // reserved scrollback rows. Scissored to the scrollback rect so a
            // partially-scrolled figure clips at the drawer edges instead of
            // bleeding over the input line or pane borders.
            if self.drawer == DrawerContent::Repl && !self.repl_image_slots.is_empty() {
                let area = self.repl_scrollback_px;
                let (win_start, win_end) = self.repl_window;
                let sw = (area.w.max(0.0) as u32).min(self.config.width);
                let sh = (area.h.max(0.0) as u32).min(self.config.height);
                if sw > 0 && sh > 0 {
                    let sx = (area.x.max(0.0) as u32).min(self.config.width - 1);
                    let sy = (area.y.max(0.0) as u32).min(self.config.height - 1);
                    let sw = sw.min(self.config.width - sx);
                    let sh = sh.min(self.config.height - sy);
                    let mut painted = false;
                    for slot in &self.repl_image_slots {
                        if slot.line + slot.rows as usize <= win_start || slot.line >= win_end {
                            continue;
                        }
                        let Some(img) = self.repl_images.get(&slot.key) else {
                            continue;
                        };
                        if !painted {
                            rpass.set_scissor_rect(sx, sy, sw, sh);
                            painted = true;
                        }
                        let rect = ScreenRect {
                            x: area.x + self.cell_w,
                            y: area.y + (slot.line as f32 - win_start as f32) * self.cell_h,
                            w: slot.disp_w,
                            h: slot.disp_h,
                        };
                        img.quad.render(
                            &self.queue,
                            &self.quad_pipeline,
                            &mut rpass,
                            rect,
                            (self.config.width, self.config.height),
                        )?;
                    }
                    if painted {
                        rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                    }
                }
            }

            // Media paint: math SVGs and figures share this pass since
            // they both ride FFFC placeholders. Paint after the file
            // preview so the bitmap sits on top of any background
            // tinting, and BEFORE text so the FFFC placeholder glyph
            // (and any default-font visual artefact) gets covered.
            //
            // `rect` here is the row reservation (full preview width by
            // the FFFC line's reserved height) for display math and
            // figures; an inline-math rect is already sized + anchored
            // to the FFFC glyph. The bitmap was sized to natural aspect
            // at rasterise time; centre it inside the rect rather than
            // stretching non-uniformly.
            //
            // Scissor the whole pass to the preview pane: a figure (or a
            // tall display-math block) whose reservation straddles the
            // pane's bottom edge would otherwise paint its lower half
            // straight into the terminal drawer below. The code-block
            // panels solve the same bleed by rect-CLAMPING (a solid quad
            // clamps cleanly); a textured image can't — clamping the dest
            // rect squashes the bitmap — so it gets the same wgpu scissor
            // the PNG canvas path uses, then reset to full-frame after.
            {
                let sx = preview_rect.x.max(0.0) as u32;
                let sy = preview_rect.y.max(0.0) as u32;
                let sw = preview_rect.w.max(0.0) as u32;
                let sh = preview_rect.h.max(0.0) as u32;
                rpass.set_scissor_rect(sx, sy, sw, sh);
            }
            for (block_idx, rect) in &media_paint_targets {
                let Some(block) = self.preview_md.media_blocks.get(*block_idx) else {
                    continue;
                };
                // Resolve the kind-specific source quad + the paint
                // size policy. Math SVGs are pre-rasterised at a size
                // that already fits the row reservation (uniform
                // downscale done at rasterise time), so the paint pass
                // just centres them inside the rect. Figures are
                // cached at natural pixel size and might exceed the
                // row reservation in either dimension; uniform-scale
                // them to fit on the paint side.
                let (quad, paint_w, paint_h): (&Quad, f32, f32) = match block {
                    crate::preview::markdown::MediaBlock::Math { latex, display } => {
                        let key = (latex.clone(), *display);
                        let Some(entry) = self.math_cache.get(&key) else {
                            continue;
                        };
                        let Some(q) = entry.rasterised.as_ref() else {
                            continue;
                        };
                        let (pw, ph) = q.size_px;
                        let pw_f = (pw as f32).min(rect.w);
                        let ph_f = (ph as f32).min(rect.h);
                        (q, pw_f, ph_f)
                    }
                    crate::preview::markdown::MediaBlock::Figure { url, .. } => {
                        let Some(entry) = self.figure_cache.get(url) else {
                            continue;
                        };
                        let pw = entry.natural_w_px as f32;
                        let ph = entry.natural_h_px as f32;
                        let s = (rect.w / pw.max(1.0))
                            .min(rect.h / ph.max(1.0))
                            .min(1.0)
                            .max(0.001);
                        (&entry.quad, pw * s, ph * s)
                    }
                    // Tables paint via the extras text path, not the
                    // quad pipeline — buffer is built before
                    // text.prepare, hosted in `extras`, with TextBounds
                    // clipping the overflow to the preview pane and
                    // `md_table_scroll_px` shifting the text left for
                    // horizontal scroll. Nothing to do in this loop.
                    crate::preview::markdown::MediaBlock::Table { .. } => continue,
                };
                // Cull rects that fall completely outside the preview
                // viewport — avoids spending pixels on offscreen media.
                if rect.y + rect.h < preview_rect.y || rect.y > preview_rect.y + preview_rect.h {
                    continue;
                }
                let paint_rect = ScreenRect {
                    x: rect.x + ((rect.w - paint_w) * 0.5).max(0.0),
                    y: rect.y + ((rect.h - paint_h) * 0.5).max(0.0),
                    w: paint_w,
                    h: paint_h,
                };
                quad.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    paint_rect,
                    (self.config.width, self.config.height),
                )?;
            }
            // Reset scissor so subsequent draws (code panels, strike
            // lines, chrome text) aren't clipped to the preview pane.
            rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);

            // LLM-pane selection highlight — render a pastel-yellow rect
            // for each row of the selection before text.render so glyphs
            // sit on top. Per-row geometry: the first and last rows may
            // be partial (start_col..pane_cols and 0..=end_col); middle
            // rows are full-width.
            if let Some(sel) = llm_selection {
                let (a, b) = sel;
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                let (sr, sc) = start;
                let (er, ec) = end;
                let pane = self.pane_rects.llm;
                if pane.width > 0 && pane.height > 0 {
                    let origin_x = self.chrome_origin_x + pane.x as f32 * self.cell_w;
                    let origin_y = self.chrome_origin_y + pane.y as f32 * self.cell_h;
                    let max_row = pane.height.saturating_sub(1);
                    let max_col = pane.width.saturating_sub(1);
                    let sr = sr.min(max_row);
                    let er = er.min(max_row);
                    let sc = sc.min(max_col);
                    let ec = ec.min(max_col);
                    // Batched: a per-row `render()` loop rewrites the same
                    // vbuf inside one render pass, so only the LAST row's
                    // highlight ever reached the GPU — a multiline drag
                    // looked like single-line selection (the copy walked
                    // the real range all along). Same fix as the markdown
                    // code-bg panels.
                    let mut row_rects: Vec<ScreenRect> = Vec::new();
                    for row in sr..=er {
                        let cs = if row == sr { sc } else { 0 };
                        let ce = if row == er { ec } else { max_col };
                        if ce < cs {
                            continue;
                        }
                        let span = (ce - cs + 1) as f32;
                        row_rects.push(ScreenRect {
                            x: origin_x + cs as f32 * self.cell_w,
                            y: origin_y + row as f32 * self.cell_h,
                            w: span * self.cell_w,
                            h: self.cell_h,
                        });
                    }
                    if !row_rects.is_empty() {
                        self.selection_bg_quad.render_many(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &mut rpass,
                            &row_rects,
                            (self.config.width, self.config.height),
                        )?;
                    }
                }
            }

            // Markdown code-bg panel — paint the slate quad behind
            // every contiguous code-glyph run the walk tagged with
            // CODE_GLYPH_META. Rects come back in buffer-local coords;
            // we add the markdown pane origin + EXTRA_TOP_PAD_PX (the
            // same headroom the text gets) and subtract the scroll so
            // the panel rides with the text on wheel. Clipped to the
            // preview rect so a code line that's just scrolled past
            // doesn't bleed into the wireframe.
            // Single batched render for both block panels (full pane
            // width) and inline pills (text width + padding). Combined
            // because `Quad::render_many` borrows `&mut self.code_bg_quad`
            // tied to the rpass lifetime, so two separate calls in the
            // same scope conflict; one merged Vec sidesteps it.
            if show_md {
                // One rect per fenced block, spanning from the first
                // line's top to the last line's bottom — covers blank
                // lines inside the fence that `code_block_line_rects`
                // skipped, so the panel reads as one continuous strip
                // rather than per-line stripes with gaps.
                let block_rects = self.preview_md.code_block_rects();
                let inline_rects = self.preview_md.code_glyph_rects();
                if !block_rects.is_empty() || !inline_rects.is_empty() {
                    const PAD_X: f32 = 3.0;
                    const PAD_Y: f32 = 1.0;
                    const BLOCK_PAD_Y: f32 = 4.0;
                    let pane_top = preview_rect.y;
                    let pane_bot = preview_rect.y + preview_rect.h;
                    let pane_left = md_rect.x;
                    let pane_right = md_rect.x + md_rect.w;
                    let mut batched: Vec<ScreenRect> =
                        Vec::with_capacity(block_rects.len() + inline_rects.len());
                    // Block panels first — full pane width per block, no
                    // x-padding; the line already covers the gutter.
                    for (by, bh) in block_rects {
                        let sy = md_rect.y + crate::text::EXTRA_TOP_PAD_PX + by
                            - preview_scroll_px
                            - BLOCK_PAD_Y;
                        let sh = bh + 2.0 * BLOCK_PAD_Y;
                        // Clamp the panel to the preview pane's visible band,
                        // not just cull: a block taller than the pane (a long
                        // HDF5 tree, say) must stop at the drawer top
                        // (`pane_bot`) instead of bleeding down into the
                        // drawer, and at `pane_top` when scrolled up.
                        let top = sy.max(pane_top);
                        let bot = (sy + sh).min(pane_bot);
                        if bot <= top {
                            continue;
                        }
                        batched.push(ScreenRect {
                            x: md_rect.x,
                            y: top,
                            w: md_rect.w,
                            h: bot - top,
                        });
                    }
                    // Inline pills — text-width + small padding. Skipped
                    // for any glyph also tagged CODE_BLOCK_FLAG (the
                    // walker filters those out).
                    for (bx, by, bw, bh) in inline_rects {
                        let sy = md_rect.y + crate::text::EXTRA_TOP_PAD_PX + by
                            - preview_scroll_px
                            - PAD_Y;
                        let sh = bh + 2.0 * PAD_Y;
                        if sy + sh < pane_top || sy > pane_bot {
                            continue;
                        }
                        let raw_x = md_rect.x + bx - PAD_X;
                        let raw_w = bw + 2.0 * PAD_X;
                        let sx = raw_x.max(pane_left);
                        let sw = (raw_x + raw_w).min(pane_right) - sx;
                        if sw <= 0.0 {
                            continue;
                        }
                        batched.push(ScreenRect {
                            x: sx,
                            y: sy,
                            w: sw,
                            h: sh,
                        });
                    }
                    if !batched.is_empty() {
                        self.code_bg_quad.render_many(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &mut rpass,
                            &batched,
                            (self.config.width, self.config.height),
                        )?;
                    }
                    // Per-block 1-px border around the slate panel —
                    // top / bottom / left / right edges. Different
                    // Quad field from `code_bg_quad`, so a second
                    // `render_many` call in this scope is fine (the
                    // borrow conflict is per-field, not per-pass).
                    // Inline pills don't get bordered; the visual
                    // affordance is only useful at panel scale.
                    // Clamped panel rect + whether the real top / bottom edge
                    // falls inside the pane. When a panel is clipped at the
                    // drawer top (or pane top on scroll), we suppress the edge
                    // at the clip line so there's no false border drawn across
                    // the drawer boundary.
                    let block_panels: Vec<(ScreenRect, bool, bool)> = self
                        .preview_md
                        .code_block_rects()
                        .into_iter()
                        .filter_map(|(by, bh)| {
                            let sy = md_rect.y + crate::text::EXTRA_TOP_PAD_PX + by
                                - preview_scroll_px
                                - BLOCK_PAD_Y;
                            let sh = bh + 2.0 * BLOCK_PAD_Y;
                            let top = sy.max(pane_top);
                            let bot = (sy + sh).min(pane_bot);
                            if bot <= top {
                                return None;
                            }
                            let top_visible = sy >= pane_top;
                            let bot_visible = sy + sh <= pane_bot;
                            Some((
                                ScreenRect {
                                    x: md_rect.x,
                                    y: top,
                                    w: md_rect.w,
                                    h: bot - top,
                                },
                                top_visible,
                                bot_visible,
                            ))
                        })
                        .collect();
                    if !block_panels.is_empty() {
                        const BORDER: f32 = 1.0;
                        let mut edges: Vec<ScreenRect> = Vec::with_capacity(block_panels.len() * 4);
                        for (r, top_visible, bot_visible) in &block_panels {
                            // Top edge — only if the real top is in-pane.
                            if *top_visible {
                                edges.push(ScreenRect {
                                    x: r.x,
                                    y: r.y,
                                    w: r.w,
                                    h: BORDER,
                                });
                            }
                            // Bottom edge — only if the real bottom is in-pane
                            // (else it'd draw a false line at the drawer top).
                            if *bot_visible {
                                edges.push(ScreenRect {
                                    x: r.x,
                                    y: r.y + r.h - BORDER,
                                    w: r.w,
                                    h: BORDER,
                                });
                            }
                            // Left / right edges span the clamped visible
                            // height (corner overlap with top/bottom is the
                            // same colour, so harmless).
                            edges.push(ScreenRect {
                                x: r.x,
                                y: r.y,
                                w: BORDER,
                                h: r.h,
                            });
                            edges.push(ScreenRect {
                                x: r.x + r.w - BORDER,
                                y: r.y,
                                w: BORDER,
                                h: r.h,
                            });
                        }
                        self.code_border_quad.render_many(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &mut rpass,
                            &edges,
                            (self.config.width, self.config.height),
                        )?;
                    }
                }
            }

            // Markdown strikethrough — thin horizontal quad at the
            // line's x-height midline for every STRIKE_GLYPH_FLAG run.
            // Replaces the combining-mark fallback that rasterised
            // inconsistently across font picks.
            if show_md {
                let rects = self.preview_md.strike_glyph_rects();
                if !rects.is_empty() {
                    const STRIKE_THICKNESS: f32 = 1.5;
                    let pane_top = preview_rect.y;
                    let pane_bot = preview_rect.y + preview_rect.h;
                    let pane_left = md_rect.x;
                    let pane_right = md_rect.x + md_rect.w;
                    let mut batched: Vec<ScreenRect> = Vec::with_capacity(rects.len());
                    for (bx, by, bw, bh) in rects {
                        // Mid-x-height is ≈ 55% down from line_top for a
                        // single-size run; close enough for the heading
                        // / paragraph mix the preview shows.
                        let line_y = md_rect.y + crate::text::EXTRA_TOP_PAD_PX + by
                            - preview_scroll_px
                            + bh * 0.55
                            - STRIKE_THICKNESS * 0.5;
                        if line_y + STRIKE_THICKNESS < pane_top || line_y > pane_bot {
                            continue;
                        }
                        let raw_x = md_rect.x + bx;
                        let sx = raw_x.max(pane_left);
                        let sw = (raw_x + bw).min(pane_right) - sx;
                        if sw <= 0.0 {
                            continue;
                        }
                        batched.push(ScreenRect {
                            x: sx,
                            y: line_y,
                            w: sw,
                            h: STRIKE_THICKNESS,
                        });
                    }
                    self.strike_line_quad.render_many(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        &batched,
                        (self.config.width, self.config.height),
                    )?;
                }
            }

            // Brand chrome: the dark logo at each ship's bow in the strip, plus
            // the wordmark at the top-right of the nav pane. Drawn just before
            // the text layer so any glyphs (e.g. the nav title) stay legible on
            // top. Cosmetic — each is skipped when its quad failed to decode
            // (field is None).
            //
            // Bow wheels via render_many, NOT a render() loop: render()
            // rewrites the quad's vbuf at offset 0, so a per-rect loop in one
            // pass leaves only the LAST rect on the GPU (see Quad::render_many's
            // own docstring) — that bug showed exactly one logo at the right end.
            if !strip_logo_rects.is_empty() {
                // Copy out before the &mut borrow of logo_quad below.
                let wheel_angle = self.wheel_angle;
                if let Some((quad, _, _)) = self.logo_quad.as_mut() {
                    quad.render_many_rotated(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &mut rpass,
                        &strip_logo_rects,
                        wheel_angle,
                        (self.config.width, self.config.height),
                    )?;
                }
            }
            // Wordmark — the nav pane's FIRST row, left-aligned (owner ruling
            // 2026-09-06). The tree body starts on the row below (`nav_rect` in
            // the draw closure is carved by the same `wordmark_quad.is_some()`),
            // so nothing is ever drawn under it. One row tall; width follows the
            // PNG's aspect, clamped to the pane's width minus a cell each side.
            if let Some((quad, nw, nh)) = self.wordmark_quad.as_ref() {
                let nav = self.pane_rects.nav;
                let nav_x = self.chrome_origin_x + nav.x as f32 * self.cell_w;
                let nav_y = self.chrome_origin_y + nav.y as f32 * self.cell_h;
                let nav_w = nav.width as f32 * self.cell_w;
                let mut wm_h = self.cell_h;
                let mut wm_w = wm_h * (*nw as f32 / (*nh).max(1) as f32);
                let max_w = (nav_w - 2.0 * self.cell_w).max(1.0);
                if wm_w > max_w {
                    wm_w = max_w;
                    wm_h = wm_w * (*nh as f32 / (*nw).max(1) as f32);
                }
                quad.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    ScreenRect {
                        x: nav_x + self.cell_w,
                        y: nav_y + (self.cell_h - wm_h) * 0.5,
                        w: wm_w,
                        h: wm_h,
                    },
                    (self.config.width, self.config.height),
                )?;
            }

            // Pane-border quads — one batched render per colour, drawn just
            // before text.render so glyphs stay legible on top. The rects come
            // from chrome::project_border_quads (arm-from-centre, gap-free
            // tiling) computed above with the same origin/scale as the chrome
            // text. `iter_mut` yields disjoint &mut Quad, so the whole map is a
            // single mutable borrow for the pass (no per-entry borrow conflict
            // like two render_many calls on one field would hit), while
            // &self.device/queue/quad_pipeline stay separate fields — the same
            // disjoint-field pattern as the code_bg / strike passes above.
            if !border_rects_by_color.is_empty() {
                for (color, quad) in self.border_quads.iter_mut() {
                    if let Some(rects) = border_rects_by_color.get(color) {
                        quad.render_many(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &mut rpass,
                            rects,
                            (self.config.width, self.config.height),
                        )?;
                    }
                }
            }

            self.text.render(&mut rpass)?;

            // Nav-spill overlay — the ONLY draws above the main text pass.
            // Backing strips first (near-opaque surface navy, one cell row
            // tall, from the nav left edge across the border cell to the
            // spilled text's end + 1 cell pad), then the overlay glyphs on
            // top via the overlay text layer. Draw order inside the pass
            // is the z-order: these cover preview images AND main-pass
            // glyphs, which is exactly what "floating over the preview"
            // means. Full-surface scissor — the segments were reach-capped
            // at collection time.
            if !self.nav_spill_segments.is_empty() {
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                let strip_rects: Vec<ScreenRect> = self
                    .nav_spill_segments
                    .iter()
                    .map(|seg| ScreenRect {
                        x: self.chrome_origin_x + seg.x as f32 * self.cell_w,
                        y: self.chrome_origin_y + seg.row as f32 * self.cell_h,
                        w: (seg.width_cells + 1) as f32 * self.cell_w,
                        h: self.cell_h,
                    })
                    .collect();
                self.overlay_back_quad.render_many(
                    &self.device,
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    &strip_rects,
                    (self.config.width, self.config.height),
                )?;
            }
            if let (Some(rect), Some((_, quad))) = (help_overlay_rect, self.help_back_quad.as_mut()) {
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                quad.render(&self.queue, &self.quad_pipeline, &mut rpass,
                    rect, (self.config.width, self.config.height))?;
            }
            if !overlay_lines.is_empty() {
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                self.text.render_overlay(&mut rpass)?;
            }
        }

        // If `--capture` is set and we've waited long enough for transport
        // events to push through, copy the swapchain texture into a CPU
        // buffer in this same encoder, before `frame.present()` consumes it.
        // --capture-preview adds a second async round-trip (preview.get for
        // a specific file) on top of the connect-time root preview. Math
        // also has to wait on the MathJax sidecar per `$$…$$` block. Give
        // it more frames so the readback is taken after the math SVGs have
        // landed and been laid out.
        let capture_target_frame = if self.capture_delay_ms > 0 {
            // Explicit override from --capture-delay-ms. Redraw loop is
            // 60 Hz, so ms * 60 / 1000.
            (self.capture_delay_ms * 60 / 1000).max(1)
        } else if self.capture_preview_armed {
            CAPTURE_FRAME * 4
        } else {
            CAPTURE_FRAME
        };
        let capture_now = self.capture_path.is_some() && self.frame_counter == capture_target_frame;
        // Ctrl+Shift+S selfie: capture the current frame to a timestamped PNG
        // without exiting. Shares the readback machinery with the --capture
        // harness path; the harness `capture_now` (one-shot + exit) wins if
        // both request a shot on the same frame.
        let selfie_target = self.selfie_pending.take();
        let capture_target = if capture_now {
            self.capture_path.clone()
        } else {
            selfie_target.clone()
        };
        let readback = if capture_target.is_some() {
            Some(stage_capture(
                &self.device,
                &mut encoder,
                &frame.texture,
                self.config.width,
                self.config.height,
            ))
        } else {
            None
        };

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        if owed_drawn {
            for (k, n) in &owed {
                self.leases.notice_seen(k, *n);
            }
            if self.leaving.is_none() {
                self.not_ended_shown = owed_line.map(|l| (l, std::time::Instant::now() + NOTIFY_STICKY));
            }
        }
        // The leaving line holds from the frame that presents it, and only
        // then are its counts acked.
        if leaving_drawn {
            if let Some(l) = self.leaving.as_mut() {
                for (h, n) in l.presented(std::time::Instant::now()) {
                    self.leases.notice_seen(&h, n);
                }
            }
        }
        self.text.trim();

        if let Some((buf, padded_bpr, unpadded_bpr)) = readback {
            let path = capture_target.unwrap();
            let is_selfie = !capture_now && selfie_target.is_some();
            match finish_capture(
                &self.device,
                buf,
                padded_bpr,
                unpadded_bpr,
                self.config.width,
                self.config.height,
                self.config.format,
                &path,
            ) {
                Ok(()) => {
                    tracing::info!(path = %path.display(), "capture wrote PNG");
                    if is_selfie {
                        self.status = format!("selfie saved: {}", path.display());
                        self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "capture failed");
                    if is_selfie {
                        self.status = format!("selfie failed: {e}");
                        self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                    }
                }
            }
            // The --capture harness exits after its one shot; a selfie is live,
            // so keep running and repaint once so the toast shows.
            if capture_now {
                self.should_exit = true;
            } else {
                self.window.request_redraw();
            }
        } else if self.capture_path.is_some() && !self.should_exit {
            // Keep redrawing so frame_counter ticks up to CAPTURE_FRAME even
            // when there are no transport events to trigger redraws.
            self.window.request_redraw();
        }

        self.frame_counter += 1;
        self.last_frame_at = Some(std::time::Instant::now());
        Ok(())
    }
}













#[cfg(test)]
mod scan_tests;
pub(crate) mod preview;
pub(crate) mod render;
use render::*;
use self::preview::concept::{
    parse_synced_against, split_frontmatter, strip_frontmatter, ConceptInfo, FILE_PARSE_MAX_RETRIES,
};
use self::preview::editor::state::EditState;
use self::preview::reply_is_current;
use self::preview::pane::{
    is_raster_preview_mime, preview_max_scroll, preview_scroll_target, resolve_preview_changed,
    resolve_previewed_path, SAMPLE_MARKDOWN,
};
