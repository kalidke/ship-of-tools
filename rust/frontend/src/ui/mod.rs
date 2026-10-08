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

pub(crate) mod chrome;
use chrome::*;
pub(crate) mod drawer;
#[cfg(windows)]
use drawer::terminal::backend::drawer_uses_attach;
use drawer::repl::lines::{ReplImage, ReplImageSlot};
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
    evt_rx: std::sync::mpsc::Receiver<(crate::net::dial::HostKey, crate::net::transport::IncomingEvt)>,
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
        crate::net::dial::HostKey,
        tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
    )>,
    // net/hosts.rs (fe-net): the per-host connection table.
    /// fe-net's per-host connection table (F/net/hosts.rs).
    hosts: crate::net::hosts::HostTable,
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
    /// `keybindings.toml` if present). See `ui/input/keybindings.rs` for the
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
    workspace_lists: HashMap<crate::net::dial::HostKey, Vec<crate::net::transport::WorkspaceInfo>>,
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
    /// Results owed to current listed canonical rows; authoritative removal or identity replacement invalidates their entries and attempts.
    pending_nav: PendingResults,
    /// Serial of the newest result recorded; never reset, so a replacement is told from the result it replaced.
    result_serial: u64,
    /// Serial of the newest attempt started; never reset.
    attempt_serial: u64,
    /// The attempt the in-flight reveal (`pending_reveal` or `pending_switch_reveal`) works for, when it
    /// is a result's; ordinary tree replies neither advance nor abort a reveal while it is set.
    result_reveal: Option<crate::net::transport::ResultAttemptId>,
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
    active_host: crate::net::dial::HostKey,
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
    /// `EXEC_EVAL_ID` is a per-process static in rust/backend/src/sidecars/repl/execute.rs
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
    local_term: Option<crate::ui::drawer::terminal::pty::LocalTerminal>,
    /// ADR 0041 step 6 U3: the attach-only alternative to `local_term`,
    /// live only when `settings.attach_only` is on (Windows only — see
    /// `sot_log::attach_client::client`). Mutually exclusive with `local_term`:
    /// the Terminal drawer's lazy-spawn site picks exactly one backend
    /// at creation time and never both. `None` on every non-Windows
    /// build target (the field itself still exists there so the rest of
    /// this struct's layout doesn't fork by platform) since attach-only
    /// has nothing to attach to off Windows.
    #[cfg(windows)]
    attach_term: Option<sot_log::attach_client::client::FeAttachClient>,
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
    highlight_service: crate::ui::preview::markdown::highlight::HighlightService,
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
    hello_refused: HashMap<HostKey, String>,
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
            crate::pages::PageDial,
            Option<String>,
            sot_protocol::topology::ssh_bridge::LinkGate,
            std::sync::Arc<crate::pages::Arm>,
        )>,
    >,
    proxy_ensured: std::collections::HashMap<u16, std::sync::Arc<crate::pages::Arm>>,
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













#[cfg(test)]
mod scan_tests;
pub(crate) mod preview;
pub(crate) mod render;
use render::*;
use self::preview::concept::{split_frontmatter, ConceptInfo, FILE_PARSE_MAX_RETRIES};
use self::preview::editor::state::EditState;
use self::preview::reply_is_current;
use self::preview::pane::{
    is_raster_preview_mime, preview_max_scroll, preview_scroll_target, resolve_preview_changed,
    resolve_previewed_path, SAMPLE_MARKDOWN,
};

#[cfg(test)]
mod result_tests {
    use super::*;

    impl State {
        pub(in crate::ui) fn result_target_uses_host_and_listed_identity(&mut self) -> Result<()> {
            let (a, b, mut a_rx, mut b_rx) = self.prepare_result_probe();
            self.result_host_probe(&b, &mut a_rx)?;
            self.result_identity_probe(&b, &mut b_rx)?;
            self.result_entry_controls(&a, &b, &mut a_rx, &mut b_rx)?;
            self.result_caption_roi_probe(&a, &b, &mut a_rx, &mut b_rx)?;
            println!("result-routing host_and_identity=true ingress_controls=true caption_roi=true entered_bodies=1 completed_bodies=1");
            Ok(())
        }

        fn prepare_result_probe(
            &mut self,
        ) -> (
            HostKey,
            HostKey,
            tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
            tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ) {
            let a = "<host-a>".to_string();
            let b = "<host-b>".to_string();
            let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
            let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
            self.conns = vec![(a.clone(), a_tx), (b.clone(), b_tx)];
            self.active_host = a.clone();
            self.active_workspace_id = Some("project".into());
            let mut row_a = ws_info("project", "stored-a");
            row_a.workspace_id = "id-a".into();
            let mut row_b = ws_info("project", "stored-b");
            row_b.workspace_id = "id-b".into();
            let mut default_a = ws_info("base-a", "default-a");
            default_a.is_default = true;
            let mut default_b = ws_info("base-b", "default-b");
            default_b.is_default = true;
            self.on_workspaces(a.clone(), vec![row_a, default_a]);
            self.on_workspaces(b.clone(), vec![row_b, default_b]);
            self.pending_nav = PendingResults::default();
            while a_rx.try_recv().is_ok() {}
            while b_rx.try_recv().is_ok() {}
            (a, b, a_rx, b_rx)
        }

        fn result_host_probe(
            &mut self,
            b: &HostKey,
            a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ) -> Result<()> {
            self.dispatch_fe_command(
                Some(&b),
                FeCommand::Preview {
                    workspace: "project".into(),
                    path: "same.png".into(),
                    urgent: false,
                    roi: Some(RoiRect {
                        x: 1,
                        y: 2,
                        w: 3,
                        h: 4,
                    }),
                    caption: Some("from b".into()),
                },
            );
            anyhow::ensure!(self.pending_result_path(b, "project").as_deref() == Some("same.png")
                && a_rx.try_recv().is_err(), "result_target_uses_host_and_listed_identity: another host's same slug rendered in the active view");
            Ok(())
        }

        fn result_identity_probe(
            &mut self,
            b: &HostKey,
            b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ) -> Result<()> {
            self.dispatch_fe_command(
                Some(&b),
                FeCommand::Reveal {
                    workspace: "id-b".into(),
                    path: "by-id.png".into(),
                    urgent: false,
                    roi: None,
                    caption: None,
                },
            );
            anyhow::ensure!(
                self.pending_result_path(b, "project").as_deref() == Some("by-id.png")
                    && !self.pending_nav.contains_key(&(b.clone(), "id-b".into())),
                "canonical spelling created a phantom strip key"
            );
            let before = (
                self.active_host.clone(),
                self.active_workspace_id.clone(),
                self.pending_nav.clone(),
            );
            self.dispatch_fe_command(
                Some(&b),
                FeCommand::Preview {
                    workspace: "unknown".into(),
                    path: "same.png".into(),
                    urgent: true,
                    roi: None,
                    caption: Some("unknown".into()),
                },
            );
            anyhow::ensure!(
                before
                    == (
                        self.active_host.clone(),
                        self.active_workspace_id.clone(),
                        self.pending_nav.clone()
                    )
                    && self.status.starts_with("result refused"),
                "unknown urgent target mutated result state"
            );
            for command in [
                FeCommand::Reveal {
                    workspace: "unknown".into(),
                    path: "same.png".into(),
                    urgent: true,
                    roi: None,
                    caption: None,
                },
                FeCommand::Workspace {
                    slug: Some("unknown".into()),
                    boot: false,
                },
            ] {
                self.dispatch_fe_command(Some(b), command);
                anyhow::ensure!(
                    before
                        == (
                            self.active_host.clone(),
                            self.active_workspace_id.clone(),
                            self.pending_nav.clone()
                        ),
                    "unknown goto or reveal mutated result state"
                );
            }
            self.dispatch_fe_command(
                Some(&b),
                FeCommand::Workspace {
                    slug: Some("id-b".into()),
                    boot: false,
                },
            );
            let requests: Vec<_> = std::iter::from_fn(|| b_rx.try_recv().ok()).collect();
            anyhow::ensure!(self.active_host == *b && self.active_workspace_id.as_deref() == Some("project")
                && requests.iter().any(|r| matches!(r, OutgoingReq::PtyOpen { target: Some(name), .. } if name == "stored-b")),
                "stored attachment name was not used for the listed canonical row");
            Ok(())
        }

        fn result_entry_controls(
            &mut self,
            a: &HostKey,
            b: &HostKey,
            a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
            b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ) -> Result<()> {
            self.cycle_workspace(1, false);
            anyhow::ensure!(
                self.bl_pane_target.as_ref() == Some(&(b.clone(), "default-b".into())),
                "cycle derived an attachment name"
            );
            self.dispatch_fe_command(
                Some(a),
                FeCommand::Workspace {
                    slug: Some("default".into()),
                    boot: false,
                },
            );
            anyhow::ensure!(
                self.active_host == *a
                    && self.active_workspace_id.is_none()
                    && self.default_workspace_slug.as_deref() == Some("base-a"),
                "default resolved using another host's list"
            );
            let envelope = parse_nav_envelope(
                r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"id-b","path":"envelope.png"}}"#,
            )
            .unwrap();
            self.handle_nav_envelope(b, &envelope);
            anyhow::ensure!(
                self.pending_result_path(b, "project").as_deref() == Some("envelope.png"),
                "envelope failed listed id resolution"
            );
            let mut selected = node("sessions:selected", "selected", false);
            selected.kind = "session".into();
            selected.payload = serde_json::json!({"host": b, "workspace_id":"id-b", "slug":"project", "name":"stored-b"}).as_object().unwrap().clone();
            self.tree
                .set_root(node("sessions:", "sessions", true), vec![selected]);
            self.tree.selected = 1;
            self.bl_pane_target = None;
            let _ = session::keys::session_enter_key(self);
            anyhow::ensure!(
                self.active_host == *b
                    && self.active_workspace_id.as_deref() == Some("project")
                    && self.bl_pane_target.as_ref() == Some(&(b.clone(), "stored-b".into())),
                "Sessions-Enter reversed a session name into a slug"
            );
            self.on_workspace_created(
                a.clone(),
                Ok(crate::net::transport::WorkspaceCreatedInfo {
                    workspace_id: "new-id".into(),
                    slug: "created".into(),
                    label: "created".into(),
                    project_root: "<root>".into(),
                    session_name: "created-target".into(),
                }),
            );
            anyhow::ensure!(std::iter::from_fn(|| a_rx.try_recv().ok()).any(|r|
                matches!(r, OutgoingReq::PtyOpen { target: Some(name), .. } if name == "created-target")), "create reply lost its supplied target");
            while b_rx.try_recv().is_ok() {}
            Ok(())
        }
        fn result_caption_roi_probe(
            &mut self,
            a: &HostKey,
            b: &HostKey,
            a_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
            b_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
        ) -> Result<()> {
            self.switch_to_workspace(a.clone(), Some("project".into()), None, false);
            let command = |workspace: &str, caption: &str| FeCommand::Preview {
                workspace: workspace.into(),
                path: "same.png".into(),
                urgent: false,
                roi: Some(RoiRect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
                caption: Some(caption.into()),
            };
            self.dispatch_fe_command(Some(a), command("project", "caption a"));
            self.dispatch_fe_command(Some(b), command("id-b", "caption b"));
            let rect = ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 240.0,
            };
            self.preview_node_id_fired = Some("files:same.png".into());
            anyhow::ensure!(
                self.build_caption(rect).is_some()
                    && self.caption_label.as_ref().unwrap().buffer.lines[0].text() == "caption a",
                "host-qualified caption render read another host's same file"
            );
            self.install_result_probe_image(a);
            anyhow::ensure!(
                self.pending_roi_aim.as_ref().is_some_and(|aim| !aim.ready),
                "another host's same file certified ROI readiness"
            );
            while a_rx.try_recv().is_ok() {}
            self.paint_result_probe_image(rect)?;
            anyhow::ensure!(
                self.pending_roi_aim.is_some() && a_rx.try_recv().is_err(),
                "another host's ROI was consumed or reported by the active renderer"
            );
            self.switch_to_workspace(b.clone(), Some("project".into()), None, false);
            self.dispatch_fe_command(Some(b), command("id-b", "caption b"));
            self.install_result_probe_image(b);
            anyhow::ensure!(
                self.pending_roi_aim.as_ref().is_some_and(|aim| aim.ready),
                "matching ROI was not certified ready"
            );
            anyhow::ensure!(
                self.build_caption(rect).is_some()
                    && self.caption_label.as_ref().unwrap().buffer.lines[0].text() == "caption b",
                "matching host caption did not reach its renderer"
            );
            while b_rx.try_recv().is_ok() {}
            self.paint_result_probe_image(rect)?;
            let reports: Vec<_> = std::iter::from_fn(|| b_rx.try_recv().ok())
                .filter_map(|req| {
                    if let OutgoingReq::AgentSend { text, .. } = req {
                        serde_json::from_str::<serde_json::Value>(&text).ok()
                    } else {
                        None
                    }
                })
                .collect();
            anyhow::ensure!(
                self.pending_roi_aim.is_none()
                    && reports.len() == 1
                    && reports[0]["evt"] == "preview_roi_applied"
                    && reports[0]["ws"] == "id-b"
                    && reports[0]["path"] == "same.png",
                "matching ROI did not apply once with its public workspace spelling"
            );
            Ok(())
        }

        fn install_result_probe_image(&mut self, host: &HostKey) {
            self.on_preview(
                host.clone(),
                Some("files:same.png".into()),
                self.active_workspace_id.clone(),
                "image/png".into(),
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../logo.png")).to_vec(),
                None,
                self.preview_req_gen,
            );
        }

        fn paint_result_probe_image(&mut self, rect: ScreenRect) -> Result<()> {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("result fixture"),
                size: wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("result fixture"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("result fixture"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    occlusion_query_set: None,
                    timestamp_writes: None,
                });
                self.paint_preview_png(&mut pass, Some(rect), rect)?;
            }
            self.queue.submit([encoder.finish()]);
            Ok(())
        }
    }

    // ---- canonical result lifetime and attempt admission (ADR 0025) ----

    type ProbeRx = tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>;
    type ProbeEvents = std::sync::mpsc::Sender<(HostKey, crate::net::transport::IncomingEvt)>;

    /// How a probe answers the tree requests State sends.
    #[derive(Clone, Copy, PartialEq)]
    enum Hold {
        Nothing,
        Children,
        Everything,
    }

    /// A tree request State sent, written through the real sender and pending map.
    #[derive(Debug)]
    struct WiredTree {
        id: u64,
        op: String,
        node_id: Option<String>,
        /// Sent as a result's own request, so its reply is the result's to complete.
        owned: bool,
    }

    struct Served {
        held: Vec<WiredTree>,
        previews: Vec<(String, u64)>,
    }

    /// The real request sender and response dispatcher, kept across the steps of one probe.
    struct TreeWire<'a> {
        runtime: tokio::runtime::Runtime,
        driver: crate::net::transport::ResultTreeTestDriver<'a>,
        next_id: u64,
    }

    fn tree_json(paths: &[&str], parent: &str) -> Vec<serde_json::Value> {
        let prefix = if parent.is_empty() {
            String::new()
        } else {
            format!("{parent}/")
        };
        let mut dirs = std::collections::BTreeSet::new();
        let mut files = std::collections::BTreeSet::new();
        for path in paths {
            if let Some(rest) = path.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((dir, _)) => {
                        dirs.insert(dir.to_string());
                    }
                    None => {
                        files.insert(rest.to_string());
                    }
                }
            }
        }
        let entry = |name: &str, has_children: bool| {
            serde_json::json!({
                "id": format!("files:{prefix}{name}"),
                "label": name,
                "kind": "files",
                "has_children": has_children,
            })
        };
        dirs.iter()
            .map(|name| entry(name, true))
            .chain(files.iter().map(|name| entry(name, false)))
            .collect()
    }

    impl<'a> TreeWire<'a> {
        fn new(host: &HostKey, events: &'a ProbeEvents) -> Result<Self> {
            Ok(Self {
                runtime: tokio::runtime::Builder::new_current_thread().build()?,
                driver: crate::net::transport::ResultTreeTestDriver::new(host.clone(), events),
                next_id: 100,
            })
        }

        /// Send a State-emitted tree request through the real sender; `None` for any other request.
        fn register(&mut self, request: OutgoingReq) -> Result<Option<WiredTree>> {
            let shape = format!("{request:?}");
            if !["ResultTree", "TreeRoot", "TreeChildren"]
                .iter()
                .any(|kind| shape.starts_with(kind))
            {
                return Ok(None);
            }
            self.next_id += 1;
            let id = self.next_id;
            let mut wire = Vec::new();
            self.runtime
                .block_on(self.driver.send(&mut wire, id, request))?;
            let (frame, _) = self.runtime.block_on(sot_protocol::codec::read_frame(
                &mut sot_protocol::codec::buffered(&wire[..]),
            ))?;
            Ok(Some(WiredTree {
                id,
                op: frame.op.clone(),
                node_id: frame
                    .payload
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                owned: shape.starts_with("ResultTree"),
            }))
        }

        fn reply_with(&mut self, tree: &WiredTree, payload: serde_json::Value) {
            self.driver
                .reply(sot_protocol::Frame::res(tree.id, &tree.op, payload));
        }

        fn reply(&mut self, tree: &WiredTree, paths: &[&str]) {
            let payload = if tree.op == "tree.root" {
                serde_json::json!({
                    "node": {"id": "files:", "label": "project", "kind": "files", "has_children": true},
                    "children": tree_json(paths, ""),
                })
            } else {
                let parent = tree
                    .node_id
                    .as_deref()
                    .and_then(|id| id.strip_prefix("files:"))
                    .unwrap_or("");
                serde_json::json!({ "children": tree_json(paths, parent) })
            };
            self.reply_with(tree, payload);
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum OldReply {
        Success,
        Error,
        Malformed,
    }

    /// One replacement case: the first and second result, whether a new result replaces the first, and how the first attempt's reply arrives.
    struct AttemptCase {
        label: &'static str,
        first: &'static str,
        second: &'static str,
        replace: bool,
        old_reply: OldReply,
        parked: bool,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Gone {
        Listing,
        Destroy,
        Replacement,
    }

    /// One removal case: how the row goes, whether an attempt is in flight when it goes, and the successor's result.
    struct LifeCase {
        label: &'static str,
        gone: Gone,
        inflight: bool,
        successor: &'static str,
    }

    type WorkspaceInfoRow = crate::net::transport::WorkspaceInfo;

    fn destroyed_info(id: &str) -> crate::net::transport::WorkspaceDestroyedInfo {
        crate::net::transport::WorkspaceDestroyedInfo {
            workspace_id: id.into(),
            slug: "project".into(),
            label: "project".into(),
            tmux_killed: true,
            toml_removed: true,
            kept: None,
        }
    }

    fn probe_row(
        slug: &str,
        workspace_id: &str,
        default: bool,
    ) -> crate::net::transport::WorkspaceInfo {
        let mut row = ws_info(slug, &format!("{slug}-session"));
        row.workspace_id = workspace_id.into();
        row.is_default = default;
        row
    }

    fn show_command(path: &str) -> FeCommand {
        FeCommand::Preview {
            workspace: "project".into(),
            path: path.into(),
            urgent: false,
            roi: None,
            caption: None,
        }
    }

    fn goto_command(slug: &str) -> FeCommand {
        FeCommand::Workspace {
            slug: Some(slug.into()),
            boot: false,
        }
    }

    impl State {
        /// A fresh host with a default row and a `project` row, routed through the fixture's own channels.
        fn prepare_lifetime_probe(
            &mut self,
            host: &str,
            project_id: &str,
        ) -> (HostKey, ProbeRx, ProbeEvents) {
            let host = host.to_string();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            self.conns.push((host.clone(), tx));
            let (events, receiver) = std::sync::mpsc::channel();
            self.evt_rx = receiver;
            self.active_host = host.clone();
            self.active_workspace_id = None;
            self.on_workspaces(
                host.clone(),
                vec![
                    probe_row("base", &format!("id-base-{host}"), true),
                    probe_row("project", project_id, false),
                ],
            );
            (host, rx, events)
        }

        /// Answer root requests at once; hold or answer the rest. Returns what is still held and the previews asked for.
        fn serve_result_requests(
            &mut self,
            wire: &mut TreeWire<'_>,
            rx: &mut ProbeRx,
            paths: &[&str],
            hold: Hold,
        ) -> Result<Served> {
            let mut served = Served {
                held: Vec::new(),
                previews: Vec::new(),
            };
            for _ in 0..8 {
                let requests: Vec<OutgoingReq> =
                    std::iter::from_fn(|| rx.try_recv().ok()).collect();
                if requests.is_empty() {
                    break;
                }
                for request in requests {
                    if let OutgoingReq::PreviewGet {
                        node_id,
                        generation,
                        ..
                    } = &request
                    {
                        served.previews.push((node_id.clone(), *generation));
                        continue;
                    }
                    let Some(tree) = wire.register(request)? else {
                        continue;
                    };
                    let held = match hold {
                        Hold::Everything => true,
                        Hold::Children => tree.op == "tree.children",
                        Hold::Nothing => false,
                    };
                    if held {
                        served.held.push(tree);
                    } else {
                        wire.reply(&tree, paths);
                    }
                }
                self.drain_events();
            }
            Ok(served)
        }

        fn probe_sees_badge(&self, host: &HostKey) -> bool {
            self.badged_keys()
                .contains(&(host.clone(), "project".into()))
        }

        fn probe_selected_id(&self) -> Option<String> {
            self.tree
                .rows
                .get(self.tree.selected)
                .map(|row| row.node.id.clone())
        }

        fn probe_install_preview(&mut self, host: &HostKey, node_id: &str, generation: u64) {
            self.on_preview(
                host.clone(),
                Some(node_id.to_string()),
                self.active_workspace_id.clone(),
                "text/plain".into(),
                b"fn shown() {}\n".to_vec(),
                None,
                generation,
            );
        }

        /// Everything an obsolete completion must leave alone.
        fn probe_effects(
            &self,
            host: &HostKey,
        ) -> (Vec<String>, usize, String, bool, bool, Option<String>, bool) {
            (
                self.tree
                    .rows
                    .iter()
                    .map(|row| row.node.id.clone())
                    .collect(),
                self.tree.selected,
                self.status.clone(),
                self.pending_reveal.is_some(),
                self.pending_switch_reveal.is_some(),
                self.reveal_awaiting.clone(),
                self.probe_sees_badge(host),
            )
        }

        fn parked_project_rows(&mut self, host: &HostKey) -> Vec<String> {
            let key: TreeKey = (
                Mode::Files,
                TreeScope::Workspace((host.clone(), "project".into())),
            );
            self.tree_store
                .slot_mut(key)
                .view
                .rows
                .iter()
                .map(|row| row.node.id.clone())
                .collect()
        }

        /// The newest attempt's preview arrives and a frame is presented: that clears the badge, and nothing earlier did.
        fn finish_probe_attempt(
            &mut self,
            label: &str,
            host: &HostKey,
            previews: &[(String, u64)],
            target_id: &str,
        ) -> Result<()> {
            let (node, generation) = previews
                .last()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{label}: the replacement asked for no preview"))?;
            anyhow::ensure!(
                node == target_id,
                "{label}: the replacement previewed {node}"
            );
            self.probe_install_preview(host, &node, generation);
            anyhow::ensure!(
                self.probe_sees_badge(host),
                "{label}: the badge cleared before any presentation"
            );
            self.redraw()?;
            anyhow::ensure!(
                !self.probe_sees_badge(host),
                "{label}: the matching presentation did not clear the badge"
            );
            Ok(())
        }

        pub(in crate::ui) fn replaced_result_rejects_old_tree_attempt_before_effects(
            &mut self,
        ) -> Result<()> {
            println!("result-attempt phase=body-entered entered_bodies=1");
            let case = |label, first, second, replace, old_reply, parked| AttemptCase {
                label,
                first,
                second,
                replace,
                old_reply,
                parked,
            };
            for case in [
                case(
                    "sibling",
                    "src/a.rs",
                    "src/b.rs",
                    true,
                    OldReply::Success,
                    false,
                ),
                case(
                    "same-node",
                    "src/b.rs",
                    "src/b.rs",
                    true,
                    OldReply::Success,
                    false,
                ),
                case(
                    "retry",
                    "src/b.rs",
                    "src/b.rs",
                    false,
                    OldReply::Success,
                    false,
                ),
                case(
                    "error",
                    "src/a.rs",
                    "src/b.rs",
                    true,
                    OldReply::Error,
                    false,
                ),
                case(
                    "malformed",
                    "src/a.rs",
                    "src/b.rs",
                    true,
                    OldReply::Malformed,
                    false,
                ),
                case(
                    "parked",
                    "src/a.rs",
                    "src/b.rs",
                    true,
                    OldReply::Success,
                    true,
                ),
            ] {
                self.replacement_case(&case)?;
                println!(
                    "result-attempt case={} admitted_once=true entered_bodies=1",
                    case.label
                );
            }
            println!("result-attempt phase=completed ok=true entered_bodies=1 completed_bodies=1");
            Ok(())
        }

        /// The old attempt's reply arrives; whatever the replacement shows must not move.
        fn deliver_old_reply(
            &mut self,
            wire: &mut TreeWire<'_>,
            old: &WiredTree,
            case: &AttemptCase,
            paths: &[&str],
        ) {
            match case.old_reply {
                OldReply::Success => wire.reply(old, paths),
                OldReply::Error => wire.reply_with(
                    old,
                    serde_json::json!({"error": "old attempt failed", "code": "x"}),
                ),
                OldReply::Malformed => wire.reply_with(old, serde_json::json!({"nonsense": 1})),
            }
            self.drain_events();
        }

        fn replacement_case(&mut self, case: &AttemptCase) -> Result<()> {
            const REJECTED: &str =
                "obsolete result tree completion changed the replacement attempt before admission";
            let label = case.label;
            let paths = ["src/a.rs", "src/b.rs"];
            let (a, mut rx, events) =
                self.prepare_lifetime_probe(&format!("<host-attempt-{label}>"), "id-attempt");
            let mut wire = TreeWire::new(&a, &events)?;
            // The first attempt waits on the children of src.
            self.dispatch_fe_command(Some(&a), show_command(case.first));
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let mut first =
                self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Children)?;
            anyhow::ensure!(
                first.held.len() == 1 && first.held[0].node_id.as_deref() == Some("files:src"),
                "{label}: the first attempt did not wait on the children of src"
            );
            let old = first.held.remove(0);
            // The row is left, then shown again for the same or a replacing result, on the same parent.
            self.switch_to_workspace(a.clone(), None, None, false);
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            if case.replace {
                self.dispatch_fe_command(Some(&a), show_command(case.second));
            }
            if case.parked {
                let before = (
                    self.parked_project_rows(&a),
                    self.tree.rows.len(),
                    self.status.clone(),
                );
                self.deliver_old_reply(&mut wire, &old, case, &paths);
                let after = (
                    self.parked_project_rows(&a),
                    self.tree.rows.len(),
                    self.status.clone(),
                );
                anyhow::ensure!(before == after, "{REJECTED}");
            }
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let second = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Children)?;
            let same_parent = |tree: &&WiredTree| tree.node_id.as_deref() == Some("files:src");
            anyhow::ensure!(
                second.held.iter().filter(same_parent).count() == 2
                    && second.held.last().is_some_and(|tree| same_parent(&tree)),
                "{label}: the replacement did not ask for the same parent: {:?}",
                second.held
            );
            let (own, refreshes) = second.held.split_last().expect("held is not empty");
            let target_id = format!("files:{}", case.second);
            // The old completion arrives while the replacement waits on the same parent.
            if !case.parked {
                let before = self.probe_effects(&a);
                self.deliver_old_reply(&mut wire, &old, case, &paths);
                anyhow::ensure!(
                    before == self.probe_effects(&a)
                        && std::iter::from_fn(|| rx.try_recv().ok()).next().is_none(),
                    "{REJECTED}"
                );
            }
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "{label}: the badge was cleared when the attempt started"
            );
            // The ordinary refreshes of the revisited tree install their rows and finish nothing of the result's.
            for refresh in refreshes {
                wire.reply(refresh, &paths);
            }
            self.drain_events();
            anyhow::ensure!(
                self.probe_selected_id().as_deref() != Some(target_id.as_str())
                    && self.probe_sees_badge(&a),
                "{label}: an ordinary tree reply completed a result-owned reveal"
            );
            // Only the replacement's own reply lands the cursor.
            wire.reply(own, &paths);
            self.drain_events();
            anyhow::ensure!(
                self.probe_selected_id().as_deref() == Some(target_id.as_str())
                    && self.probe_sees_badge(&a),
                "{label}: the attempt's own reply did not land the cursor, or cleared the badge"
            );
            self.finish_probe_attempt(label, &a, &second.previews, &target_id)
        }

        /// The picker sends its selected account by name, wherever "default" sits in the list.
        pub(in crate::ui) fn first_named_account_is_sent(&mut self) -> Result<()> {
            println!("account-choice phase=body-entered entered_bodies=1");
            let (a, mut rx, _events) = self.prepare_lifetime_probe("<host-account>", "id-account");
            while rx.try_recv().is_ok() {}
            let cases: [(&str, &[&str], usize, Option<&str>); 7] = [
                ("no default", &["team", "other"], 0, Some("team")),
                ("default first, default chosen", &["default", "team"], 0, None),
                ("default first, second chosen", &["default", "team"], 1, Some("team")),
                ("default later, first chosen", &["team", "default"], 0, Some("team")),
                ("default later, default chosen", &["team", "default"], 1, None),
                ("empty list", &[], 0, None),
                ("stale index", &["default", "team"], 5, None),
            ];
            for (label, names, selected, want) in cases {
                self.begin_create_session(a.clone());
                let accounts = names
                    .iter()
                    .map(|name| crate::net::transport::AccountInfo {
                        name: name.to_string(),
                        kinds: Vec::new(),
                        logged_in: Default::default(),
                    })
                    .collect();
                let picker = self.workspace_picker.as_mut().ok_or_else(|| anyhow::anyhow!("{label}: no picker"))?;
                picker.accounts = accounts;
                picker.account_selected = selected;
                self.picker_confirm_selected("claude");
                self.picker_cancel();
                let sent = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|request| match request {
                    OutgoingReq::WorkspaceCreate { account, .. } => Some(account),
                    _ => None,
                });
                anyhow::ensure!(
                    sent == Some(want.map(str::to_string)),
                    "first_named_account_is_sent: {label}: sent {sent:?}, wanted {want:?}"
                );
            }
            println!("account-choice phase=completed ok=true entered_bodies=1 completed_bodies=1");
            Ok(())
        }

        pub(in crate::ui) fn passing_a_result_row_keeps_its_badge(&mut self) -> Result<()> {
            println!("result-badge phase=body-entered entered_bodies=1");
            let (a, mut rx, events) = self.prepare_lifetime_probe("<host-badge>", "id-badge");
            let mut wire = TreeWire::new(&a, &events)?;
            let paths = ["top.rs", "other.rs"];
            // Preview first, cursor second: neither alone clears the badge.
            self.dispatch_fe_command(Some(&a), show_command("top.rs"));
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let served =
                self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "passing_a_result_row_keeps_its_badge: the badge was cleared when the attempt started"
            );
            let (node, generation) = served
                .previews
                .last()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
            self.probe_install_preview(&a, &node, generation);
            self.redraw()?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "a matching preview without its cursor cleared the badge"
            );
            for tree in &served.held {
                wire.reply(tree, &paths);
            }
            self.drain_events();
            self.redraw()?;
            anyhow::ensure!(
                !self.probe_sees_badge(&a),
                "cursor and preview with a presentation did not clear the badge"
            );
            self.redraw()?;
            anyhow::ensure!(
                !self.probe_sees_badge(&a),
                "a later frame brought the badge back"
            );
            // A failed preview, a stale generation and another row's frame keep the next attempt's badge.
            self.switch_to_workspace(a.clone(), None, None, false);
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            self.dispatch_fe_command(Some(&a), show_command("top.rs"));
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let served = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            let (node, generation) = served
                .previews
                .last()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
            self.redraw()?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "a cursor without its preview cleared the badge"
            );
            self.on_preview_get_failed(
                a.clone(),
                Some(node.clone()),
                self.active_workspace_id.clone(),
                generation,
                "kernel_unavailable".into(),
            );
            self.redraw()?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "a failed preview cleared the badge"
            );
            self.probe_install_preview(&a, &node, generation.saturating_sub(1));
            self.redraw()?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "a stale preview generation cleared the badge"
            );
            self.switch_to_workspace(a.clone(), None, None, false);
            self.redraw()?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "another row's presented frame cleared the badge"
            );
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            // Returning retries with the replacing result and clears it once presented.
            self.dispatch_fe_command(Some(&a), show_command("other.rs"));
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let served = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            let (node, generation) = served
                .previews
                .last()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no preview asked for"))?;
            anyhow::ensure!(
                node == "files:other.rs",
                "the retry previewed {node}, not the replacing result"
            );
            self.probe_install_preview(&a, &node, generation);
            self.redraw()?;
            anyhow::ensure!(
                !self.probe_sees_badge(&a),
                "the retry's presentation did not clear the badge"
            );
            println!("result-badge phase=completed ok=true entered_bodies=1 completed_bodies=1");
            Ok(())
        }

        /// The successor's own completions acknowledge it once.
        fn finish_probe_successor(
            &mut self,
            label: &str,
            host: &HostKey,
            wire: &mut TreeWire<'_>,
            rx: &mut ProbeRx,
            paths: &[&str],
            new: &Served,
        ) -> Result<()> {
            for tree in &new.held {
                wire.reply(tree, paths);
            }
            self.drain_events();
            self.serve_result_requests(wire, rx, paths, Hold::Nothing)?;
            let (node, generation) = new
                .previews
                .last()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{label}: no preview"))?;
            self.probe_install_preview(host, &node, generation);
            self.redraw()?;
            anyhow::ensure!(
                !self.probe_sees_badge(host),
                "{label}: the successor's presentation did not clear its result"
            );
            Ok(())
        }

        pub(in crate::ui) fn pending_result_dies_with_its_canonical_row(&mut self) -> Result<()> {
            println!("result-lifetime phase=body-entered entered_bodies=1");
            let case = |label, gone, inflight, successor| LifeCase {
                label,
                gone,
                inflight,
                successor,
            };
            for case in [
                case("listing", Gone::Listing, false, "other.rs"),
                case("replacement", Gone::Replacement, false, "other.rs"),
                case("listing-inflight", Gone::Listing, true, "other.rs"),
                case(
                    "listing-inflight-same-path",
                    Gone::Listing,
                    true,
                    "src/a.rs",
                ),
                case("destroy-inflight", Gone::Destroy, true, "other.rs"),
                case("replacement-inflight", Gone::Replacement, true, "src/a.rs"),
            ] {
                self.removal_case(&case)?;
                println!("result-lifetime case={} entered_bodies=1", case.label);
            }
            self.pending_result_controls()?;
            println!("result-lifetime phase=completed ok=true entered_bodies=1 completed_bodies=1");
            Ok(())
        }

        /// The row goes (by a list, a destroy or a same-slug replacement) with its result owed or in flight.
        fn remove_probe_row(
            &mut self,
            case: &LifeCase,
            host: &HostKey,
            base: &WorkspaceInfoRow,
        ) -> Result<()> {
            let label = case.label;
            match case.gone {
                Gone::Destroy => {
                    self.on_workspace_destroyed(host.clone(), Err("refused".into()));
                    anyhow::ensure!(
                        self.pending_reveal.is_some() || self.pending_switch_reveal.is_some(),
                        "{label}: a failed destroy aborted the attempt"
                    );
                    self.on_workspace_destroyed(host.clone(), Ok(destroyed_info("old-id")));
                }
                Gone::Listing => self.on_workspaces(host.clone(), vec![base.clone()]),
                Gone::Replacement => self.on_workspaces(
                    host.clone(),
                    vec![base.clone(), probe_row("project", "new-id", false)],
                ),
            }
            let gone = "removed canonical row retained its pending result or same-slug successor inherited it";
            anyhow::ensure!(
                !self
                    .badged_keys()
                    .contains(&(host.clone(), "project".into()))
                    && !(case.inflight
                        && (self.pending_reveal.is_some() || self.pending_switch_reveal.is_some())),
                "{gone}"
            );
            // A successor row with the same slug is listed.
            if case.gone != Gone::Replacement {
                self.on_workspaces(
                    host.clone(),
                    vec![base.clone(), probe_row("project", "new-id", false)],
                );
            }
            anyhow::ensure!(!self.probe_sees_badge(host), "{gone}");
            Ok(())
        }

        fn removal_case(&mut self, case: &LifeCase) -> Result<()> {
            let label = case.label;
            let paths = ["src/a.rs", "other.rs"];
            let host = format!("<host-life-{label}>");
            let (a, mut rx, events) = self.prepare_lifetime_probe(&host, "old-id");
            let mut wire = TreeWire::new(&a, &events)?;
            let base = probe_row("base", &format!("id-base-{host}"), true);
            self.dispatch_fe_command(Some(&a), show_command("src/a.rs"));
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "{label}: the result was not owed"
            );
            let mut old = Served {
                held: Vec::new(),
                previews: Vec::new(),
            };
            if case.inflight {
                self.dispatch_fe_command(Some(&a), goto_command("project"));
                old = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
                anyhow::ensure!(
                    !old.held.is_empty(),
                    "{label}: the first attempt did not start"
                );
            }
            self.remove_probe_row(case, &a, &base)?;
            self.switch_to_workspace(a.clone(), None, None, false);
            self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Nothing)?;
            self.dispatch_fe_command(Some(&a), show_command(case.successor));
            self.dispatch_fe_command(Some(&a), goto_command("project"));
            let new = self.serve_result_requests(&mut wire, &mut rx, &paths, Hold::Everything)?;
            anyhow::ensure!(
                self.probe_sees_badge(&a),
                "{label}: the successor's result was not owed"
            );
            // Everything the removed row's attempt still had in flight arrives now.
            let before = self.probe_effects(&a);
            for tree in old.held.iter().filter(|tree| tree.owned) {
                wire.reply(tree, &paths);
            }
            self.drain_events();
            if let Some((old_node, old_generation)) = old.previews.last() {
                self.probe_install_preview(&a, old_node, *old_generation);
            }
            self.redraw()?;
            let after = self.probe_effects(&a);
            anyhow::ensure!(
                before.0 == after.0
                    && before.1 == after.1
                    && self.probe_sees_badge(&a)
                    && std::iter::from_fn(|| rx.try_recv().ok()).next().is_none(),
                "{label}: a late completion of the removed row changed its successor"
            );
            self.finish_probe_successor(label, &a, &mut wire, &mut rx, &paths, &new)
        }

        /// What leaves an owed result alone: an unchanged list, another host's row with the same id, a
        /// disconnect, a kept default row and a late destroy of a replaced id.
        fn pending_result_controls(&mut self) -> Result<()> {
            let (a, _rx_a, _events_a) =
                self.prepare_lifetime_probe("<host-control-a>", "shared-id");
            let (b, _rx_b, _events_b) =
                self.prepare_lifetime_probe("<host-control-b>", "shared-id");
            self.dispatch_fe_command(Some(&a), show_command("one.rs"));
            self.dispatch_fe_command(Some(&b), show_command("two.rs"));
            anyhow::ensure!(
                self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
                "control: results were not owed"
            );
            let rows = |host: &str, id: &str| {
                vec![
                    probe_row("base", &format!("id-base-{host}"), true),
                    probe_row("project", id, false),
                ]
            };
            self.on_workspaces(a.clone(), rows("<host-control-a>", "shared-id"));
            self.on_disconnected(a.clone(), "link lost".into());
            self.on_workspace_destroyed(
                a.clone(),
                Ok(crate::net::transport::WorkspaceDestroyedInfo {
                    workspace_id: "id-base-<host-control-a>".into(),
                    slug: "base".into(),
                    label: "base".into(),
                    tmux_killed: false,
                    toml_removed: false,
                    kept: Some("run ended".into()),
                }),
            );
            anyhow::ensure!(
                self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
                "control: an unchanged list, a disconnect or a kept default row removed an owed result"
            );
            // The same slug and id on one host is replaced; the other host's row keeps its result.
            self.on_workspaces(a.clone(), rows("<host-control-a>", "second-id"));
            anyhow::ensure!(
                !self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
                "control: replacing a row on one host touched the other host's row with the same id"
            );
            self.dispatch_fe_command(Some(&a), show_command("three.rs"));
            self.on_workspace_destroyed(a.clone(), Ok(destroyed_info("shared-id")));
            anyhow::ensure!(
                self.probe_sees_badge(&a) && self.probe_sees_badge(&b),
                "control: a late destroy of the replaced id removed its successor's result"
            );
            println!("result-lifetime controls=true entered_bodies=1");
            Ok(())
        }
    }
}
