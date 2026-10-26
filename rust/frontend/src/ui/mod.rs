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
    FigureMetricsMap, MarkdownPreview, MathMetricsMap,
};
use crate::ui::preview::image::png::quad_from_png_bytes;
use crate::ui::render::quad::{Quad, QuadPipeline, ScreenRect};
use crate::ui::preview::image::svg::quad_from_svg_bytes;
use crate::ui::preview::markdown::media::{
    parse_math_svg_dims, whole_row_bottom, MathSvg, MATHJAX_EX_FACTOR,
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
use preview::image::figures::decode_figure_bytes;
#[cfg(test)]
use preview::image::overlay::CAPTION_MAX_CHARS;
use preview::image::overlay::{
    parse_physical_scale, truncate_caption,
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
mod host_view;
mod page_proxy;
mod proxy_arms;
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
    /// One-line status string for the chrome, and the hold a pushed notify keeps on it.
    status: StatusLine,
    /// Selected-session contrast lever (`--contrast-mode`, ADR 0023). `false`
    /// = "bright" (default): the selected/active row pops by going brighter +
    /// bold. `true` = "dim": non-selected rows are dimmed so the selection
    /// pops by contrast. Applied in both the nav rows and the bottom strip.
    contrast_dim: bool,
    /// The battery readout's cache.
    battery: BatteryGauge,
    /// Window-wide focus, prompt editing and close notices; runtime state only.
    focus_prompts: FocusPrompts,
    /// The four pane content rects from the most recent redraw, cached
    /// so keyboard handlers can size scroll steps to a real viewport
    /// (`PgUp/PgDn`, `Ctrl+u/d`). Updated at the end of every redraw.
    pane_rects: PaneRects,
    /// Runtime drawer, layout flags, Help browsing, origin and startup requests.
    layout_help: LayoutHelp,
    /// The nav spill: its deadline, the segments the last draw collected and the cursor its trigger last saw.
    nav_spill: NavSpill,
    // ui/chrome/strip: the session strip's scroll and wheel animation.
    /// The session strip's eased scroll and the brand wheels' spin.
    strip_motion: StripMotion,
    // ui/input: the last key, pointer state, key bindings and the help drawer.
    /// Label of the most recent key press (for chrome feedback). `None` until
    /// the user hits a key; modes-mode + tree navigation hang off the same
    /// keyboard input plumbing once they land.
    last_key: Option<String>,
    /// The pointer's last position and the wheel's sub-row residue.
    pointer: crate::ui::input::pointer::Pointer,
    /// Resolved keybindings (defaults overlaid with the user's
    /// `keybindings.toml` if present). See `ui/input/keybindings.rs` for the
    /// file format and discovery order. Read-only once loaded — the
    /// chrome doesn't reload mid-session.
    bindings: KeyBindings,
    help_back_quad: Option<(u8, Quad)>,
    // ui/control: the fe-commands queue and the fe-state.json signature.
    /// The fe-commands queue the watcher fills and the last fe-state.json signature.
    control: ControlChannel,
    // ui/nav: the active tree and mode, cursor-driven preview firing, request generations.
    /// The nav pane's trees: the active (mode, row)'s tree with its scroll and scan root, and the store parking
    /// every other tree. Installs route by the reply's key, so the active view can only hold the active key's tree.
    nav: Navigation,
    /// Where the nav cursor lands and when its follow fires: the settle debounce, the startup, relaunch and
    /// reconnect placements, the README default, the pin and the per-row project-scan clock.
    nav_cursor: NavCursor,
    /// Request clocks, the fired target and its re-raster, concept follow, the driven-preview hold, and the crop,
    /// scale-save and `--roi` aim transactions.
    preview_requests: preview::requests::PreviewRequests,
    // ui/nav/files: nav prompts, the reveal walk and file transfers.
    /// The Files tree's in-flight file transactions: the Ctrl+N create and Ctrl+D delete awaiting their
    /// replies, and the upload batch with its one file in flight (chunks paced by its acks, pinned to its host).
    file_ops: FileOps,
    /// The reveal walk: the target the Files tree is being opened toward (a driven open whose row is not yet
    /// visible), the level it awaits, its one force-refresh, and a target armed for a row's incoming root. The nav
    /// cursor lands on the target when its row materializes, so one command drives both panes.
    reveal: RevealWalk,
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
    /// The active connection's hello facts: host label, project root and its basename, backend version, revision.
    active_link: crate::ui::host_view::ActiveLink,
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
    /// Ctrl+M server-monitor drawer (ADR 0020): its chart, raster, rect, subscription and hub.
    monitor: crate::ui::drawer::monitor::MonitorPane,
    /// Primary monitor aspect ratio captured at startup (width /
    /// height). Used to resolve `settings.preset = "auto"` to a named
    /// preset. Locked for the session — resizing the window doesn't
    /// re-pick a different preset; the user explicitly avoided
    /// in-session reflow.
    monitor_aspect: f32,
    /// Window-wide REPL images, pixel geometry and line-build anchor.
    repl_presentation: ReplPresentation,
    // ui/drawer/terminal: the terminal drawer's backends.
    /// The Terminal drawer's local shell, its last pty size and the repo root it starts in.
    term_drawer: crate::ui::drawer::terminal::drawer::TerminalDrawer,
    // ui/agent_pane: selected client, warm storage, screen, input and receipt owner.
    agent_pane: AgentPane,
    // ui/preview: the preview pane's buffers, concept slot, scroll and overlays.
    /// The shaped preview: the flowed text buffer, its rect, scroll and definition-line anchor, the math SVG, the
    /// refused-hello overlay and the highlighter.
    shaped: ShapedPreview,
    /// The concept slot: the last annotation read, its shaped body and the rect it is shaped for.
    concept: ConceptSlot,
    /// Each host's refused hello, the active host's projected to the blocking overlay.
    hello_refused: crate::ui::host_view::HelloRefusals,
    /// The installed source: bytes, their node, their scale and the markdown origin, repainted from on a rescale or
    /// a row's restore without a round trip.
    preview_source: preview::source::InstalledPreviewSource,
    // ui/preview/image: the image view, ROI, pages, scalebar, captions and figures.
    /// The raster view: the shown raster, its zoom and pan, the visible ROI and the same-directory view carry.
    raster: RasterView,
    /// Pagination of the shown preview: page and count, the page bitmap's raster zoom and a re-raster's keep-view.
    pages: PageView,
    /// The figure overlays: the scalebar toggle and label, the caption store and label, and the caption band.
    overlays: FigureOverlays,
    /// The markdown figures: fetched bitmaps, requests in flight and terminal failures.
    figures: FigureStore,
    // ui/preview/markdown: tables, math and token caches, and reflow.
    /// The markdown media: table buffers and their scroll, math and token caches, and the pending reflow.
    md_media: MarkdownMedia,
    // ui/preview/editor: the annotation editor and file-parse staleness.
    /// The drift badge's cache: per `.jl` path (the suffix of `files:` node ids) its `file.parse` hash, the
    /// one-shot parse latch and failed parses' backoff (2026-07-02: a fast-failing kernel once drew ~4.7k
    /// file.parse/s). Lazy, per file, on view; a `preview.changed` for the file, a row change and a reconnect drop it.
    drift: DriftCheck,
    /// The preview pane's editor: the active annotation or file edit (`e` enters, `Esc` discards; a save carries
    /// its `expected_ast_hash` or file version so the backend's stale gate engages), the file edit awaiting its
    /// `file.read`, and the shaped buffer drawn in place of the file preview.
    editor: Editor,
    // ui/persist: settings.
    /// User-tunable chrome settings (layout proportions today, future
    /// general settings). Loaded once at startup via the same layered
    /// discovery as `bindings`; not re-read on file change.
    settings: Settings,
    // pages.rs (pages): page-proxy arming.
    /// ADR 0035 daemon TCP proxy — frontend half: the proxied hosts, the listener manager and the armed listeners.
    proxies: crate::ui::proxy_arms::ProxyArms,
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
use self::preview::concept::{split_frontmatter, ConceptInfo, ConceptSlot, DriftCheck, DriftStatus};
use self::preview::editor::state::{EditState, Editor};
use self::preview::shaped::ShapedPreview;
use self::preview::image::raster::RasterView;
use self::preview::image::pages::PageView;
use self::preview::image::figure_overlays::FigureOverlays;
use self::preview::image::figure_store::FigureStore;
use self::preview::markdown::media_store::MarkdownMedia;
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
